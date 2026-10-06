-- Weak tables, ephemerons, finalizers, and warnings (Phase 3.27,
-- ADR 0046 to ADR 0049). Automatic collection is stopped, so only the
-- explicit collections here run, in Lua and in Moonseed alike; garbage is
-- made inside calls, so no register keeps it. Needs the host's newud for
-- the userdata part. Warnings print as "[warn] text".

collectgarbage("stop")
local log = {}
local function say(...) log[#log + 1] = table.concat({...}, " ") end
local function flush(tag)
  print(tag, table.concat(log, ", "))
  log = {}
end
local function count(t)
  local n = 0
  for _ in pairs(t) do n = n + 1 end
  return n
end
local function gc() collectgarbage() end
local function finmt(name) return {__gc = function(o) say(name) end} end
local function garbage(f, ...) f(...) end

-- A. Registration and order.
garbage(function()
  setmetatable({}, finmt("a"))
  setmetatable({}, finmt("b"))
  setmetatable({}, finmt("c"))
end)
gc()
flush("A order")
-- __gc added after setmetatable does not register.
garbage(function()
  local mt = {}
  setmetatable({}, mt)
  mt.__gc = function() say("late") end
end)
gc() gc()
flush("A late")
-- Setting the metatable again, once it has __gc, does.
garbage(function()
  local mt = {}
  local t = setmetatable({}, mt)
  mt.__gc = function() say("reset") end
  setmetatable(t, mt)
end)
gc()
flush("A reset")
-- Re-setting the metatable of a registered object keeps its place.
local first = setmetatable({}, finmt("first"))
local second = setmetatable({}, finmt("second"))
setmetatable(first, finmt("first again"))
first, second = nil, nil
gc()
flush("A keep place")
-- debug.setmetatable registers tables and userdata, not other types.
garbage(function()
  debug.setmetatable({}, finmt("debug table"))
  debug.setmetatable(newud(4), finmt("debug userdata"))
end)
debug.setmetatable(10, {__gc = function() say("number") end})
gc()
debug.setmetatable(10, nil)
flush("A debug")
-- (A __gc that is not a function warns with an error message whose
-- wording is Moonseed's: tested in Rust.)
-- A __gc removed before the turn comes: nothing is called.
garbage(function()
  local mt_b = {__gc = function() say("b ran") end}
  local b = setmetatable({}, mt_b)
  setmetatable({}, {__gc = function() say("c ran") mt_b.__gc = nil end})
end)
gc()
flush("A lookup")
-- A __gc replaced before the turn comes: the new one is called.
garbage(function()
  local mt = {__gc = function() say("old") end}
  setmetatable({}, mt)
  mt.__gc = function() say("new") end
end)
gc()
flush("A replaced")

-- B. Resurrection and registering again.
local saved
garbage(function()
  setmetatable({x = 7}, {__gc = function(o) say("save") saved = o end})
end)
gc()
print("B saved", saved.x, getmetatable(saved) ~= nil)
saved = nil
gc()
flush("B once")
local rounds = 0
garbage(function()
  local mt = {}
  mt.__gc = function(o)
    rounds = rounds + 1
    say("round " .. rounds)
    if rounds < 3 then setmetatable(o, mt) end
  end
  setmetatable({}, mt)
end)
gc() gc() gc() gc()
flush("B again")
-- What a resurrected object reaches survives with it.
local held
garbage(function()
  local inner = {value = "inner"}
  setmetatable({inner = inner}, {__gc = function(o) held = o.inner end})
end)
gc()
print("B reach", held.value)
held = nil

-- C. Weak values.
local wv = setmetatable({}, {__mode = "v"})
garbage(function()
  wv[1] = {}
  wv[2] = ("x"):rep(60)
  wv[3] = 42
  wv[4] = true
  wv[5] = print
  wv[6] = function() end
  wv[7] = coroutine.create(function() end)
  wv[8] = newud(1)
  wv.s = "short"
  wv[9] = string.gmatch("a", "a")
end)
gc()
local present = {}
for i = 1, 9 do present[#present + 1] = tostring(wv[i] ~= nil) end
print("C values", table.concat(present, " "), wv.s, #wv[2])
-- Keys stay strong in a weak-value table.
local wvk = setmetatable({}, {__mode = "v"})
garbage(function() wvk[{}] = 1 end)
gc()
print("C strong keys", count(wvk))

-- D. Weak keys: ephemerons.
local wk = setmetatable({}, {__mode = "k"})
local alive = {}
garbage(function()
  wk[alive] = "kept"
  wk[{}] = "gone"
  wk["str"] = {}
  wk[1] = {}
  wk[2.5] = {}
  wk[true] = {}
  local cycle = {}
  wk[cycle] = {back = cycle}
end)
gc()
print("D keys", count(wk), wk[alive], type(wk.str), type(wk[1]), type(wk[2.5]), type(wk[true]))
-- A chain across tables lives while its first key does.
local e1 = setmetatable({}, {__mode = "k"})
local e2 = setmetatable({}, {__mode = "k"})
local e3 = setmetatable({}, {__mode = "k"})
local root = {}
garbage(function()
  local k2, k3 = {}, {}
  e3[k3] = "end of chain"
  e2[k2] = k3
  e1[root] = k2
end)
gc()
print("D chain", count(e1), count(e2), count(e3))
root = nil
gc()
print("D chain dead", count(e1), count(e2), count(e3))
-- A long chain inside one table, built backwards.
local same = setmetatable({}, {__mode = "k"})
local head = {}
garbage(function()
  local keys = {head}
  for i = 2, 200 do keys[i] = {} end
  for i = 200, 2, -1 do same[keys[i - 1]] = keys[i] end
end)
gc()
print("D same table", count(same))
head = nil
gc()
print("D same table dead", count(same))
-- Ephemeron cycle with no outside reference.
local ec = setmetatable({}, {__mode = "k"})
garbage(function()
  local a, b = {}, {}
  ec[a] = b
  ec[b] = a
end)
gc()
print("D cycle", count(ec))

-- E. Weak keys and values.
local wkv = setmetatable({}, {__mode = "kv"})
local kk, vv = {}, {}
garbage(function()
  wkv[kk] = vv
  wkv[{}] = vv
  wkv[kk] = nil
  wkv[kk] = {}
  wkv.x = vv
  wkv.y = {}
  wkv[1] = "one"
end)
gc()
print("E both", count(wkv), wkv.x == vv, wkv.y, wkv[1])

-- F. Modes: odd strings, changes, a shared metatable.
for _, mode in ipairs({"x", "", "kk", "vk", ("k"):rep(41), "\0k", "kx\0v", 1}) do
  local t = setmetatable({}, {__mode = mode})
  garbage(function() t[1] = {} t[{}] = 1 end)
  gc()
  print("F mode", type(mode), #tostring(mode), count(t))
end
local shared = {}
local s1 = setmetatable({}, shared)
local s2 = setmetatable({}, shared)
garbage(function() s1[1] = {} s2[1] = {} end)
gc()
print("F strong", count(s1), count(s2))
shared.__mode = "v"
gc()
print("F now weak", count(s1), count(s2))
garbage(function() s1[1] = {} end)
shared.__mode = nil
gc()
print("F strong again", count(s1))
local switch = setmetatable({}, {__mode = "k"})
garbage(function() switch[{}] = 1 switch[2] = {} end)
getmetatable(switch).__mode = "v"
gc()
print("F k to v", count(switch))

-- G. next across a collection.
local wn = setmetatable({}, {__mode = "k"})
local keep = {}
for i = 1, 5 do keep[i] = {} wn[keep[i]] = i end
garbage(function() for i = 1, 5 do wn[{}] = -i end end)
-- The key `next` returned is held, so it stays even if it was garbage.
local seen, held = 0, nil
local k, v = next(wn)
while k do
  seen = seen + 1
  if seen == 2 then gc() held = wn[k] == v end
  k, v = next(wn, k)
end
gc()
local kept = 0
for i = 1, 5 do if wn[keep[i]] == i then kept = kept + 1 end end
print("G next", kept, held, seen >= 5, count(wn))
wn[{}] = 6
print("G insert after", count(wn))

-- H. Weak tables and finalizers.
local weakv = setmetatable({}, {__mode = "v"})
local weakk = setmetatable({}, {__mode = "k"})
garbage(function()
  local o = setmetatable({}, {__gc = function(o)
    say("in gc: weak value " .. tostring(weakv[1]))
    say("in gc: weak key " .. tostring(weakk[o]))
  end})
  weakv[1] = o
  weakk[o] = "meta"
end)
gc()
flush("H during")
print("H after", count(weakv), count(weakk))
gc()
print("H next cycle", count(weakk))
-- An ephemeron's value reached only through the finalized key.
local eph = setmetatable({}, {__mode = "k"})
garbage(function()
  local o = setmetatable({}, {__gc = function(o) say("data " .. eph[o].v) end})
  eph[o] = {v = "kept for gc"}
end)
gc()
flush("H ephemeron")
-- A weak table reached only through a resurrected object.
local wt_holder
garbage(function()
  local inner_weak = setmetatable({}, {__mode = "v"})
  inner_weak[1] = {}
  inner_weak[2] = "str"
  setmetatable({w = inner_weak}, {__gc = function(o) wt_holder = o.w say("w " .. count(o.w)) end})
end)
gc()
flush("H resurrected weak")
print("H resurrected weak after", count(wt_holder))
wt_holder = nil

-- I. The finalizer's context.
garbage(function() setmetatable({}, {__gc = function()
  say("yieldable " .. tostring(coroutine.isyieldable()))
  local _, main = coroutine.running()
  say("main " .. tostring(main))
  say("collect " .. tostring(collectgarbage()))
  say("count " .. tostring(collectgarbage("count")))
end}) end)
gc()
flush("I main")
local co = coroutine.create(function()
  garbage(function() setmetatable({}, {__gc = function()
    local th, main = coroutine.running()
    say("main " .. tostring(main))
    say("yieldable " .. tostring(coroutine.isyieldable()))
  end}) end)
  collectgarbage()
  coroutine.yield("after")
end)
print("I coroutine", coroutine.resume(co))
flush("I coroutine")
garbage(function() setmetatable({}, {__gc = function() coroutine.yield() end}) end)
local cy = coroutine.wrap(function() collectgarbage() return "no yield" end)
print("I yield", cy())

-- J. Errors become warnings; the rest go on.
garbage(function()
  setmetatable({}, {__gc = function() say("j1") end})
  setmetatable({}, {__gc = function() error("string error", 0) end})
  setmetatable({}, {__gc = function() error(42) end})
  setmetatable({}, {__gc = function() error({}) end})
  setmetatable({}, {__gc = function() error() end})
  setmetatable({}, {__gc = function() say("j6") end})
end)
print("J xpcall", xpcall(gc, function(m) return "handler " .. tostring(m) end))
flush("J")

-- K. warn.
warn("one")
warn("two ", "pieces ", 3, " ", 4.5)
warn("@on")
warn("zero\0cut", " next")
print("K", pcall(warn))
print("K", pcall(warn, "a", {}))
print("K", pcall(warn, nil))
print("K", select("#", warn("x")))

-- L. Full userdata.
garbage(function()
  local u = newud(8, 1)
  debug.setuservalue(u, "uv")
  debug.setmetatable(u, {__gc = function(o) say("ud " .. debug.getuservalue(o)) end})
end)
gc()
flush("L")

-- N. A key a finished loop left in a dead register keeps nothing.
local en = setmetatable({}, {__mode = "k"})
local nh = {}
local function nchain() local k = nh for i = 1, 50 do local nk = {} en[k] = nk k = nk end end
nchain()
collectgarbage()
local nc = 0 for _ in pairs(en) do nc = nc + 1 end
print("N", nc)
nh = nil
collectgarbage()
nc = 0 for _ in pairs(en) do nc = nc + 1 end
print("N dead", nc)

-- M. collectgarbage("step") until a cycle ends and runs the finalizer
-- (how many steps that takes is the collector's timing), and an object
-- still alive at the end.
-- A step ends a cycle only in incremental mode, so M runs in it.
local mode = collectgarbage("incremental")
garbage(function() setmetatable({}, finmt("step")) end)
local steps_ended = false
repeat
  steps_ended = collectgarbage("step")
until steps_ended and #log > 0
print("M step", steps_ended)
flush("M")
collectgarbage(mode)
setmetatable({}, {__gc = function() print("closing: first registered") end})
setmetatable({}, {__gc = function() print("closing: error next") error("at close", 0) end})
setmetatable({}, {__gc = function(o)
  print("closing: last registered")
  setmetatable(o, getmetatable(o))
end})
print("end")
