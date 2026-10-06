-- The coroutine library, printed for comparison with Lua 5.4.9 (Phase
-- 3.25, ADR 0041). Run as a file named `corpus_coroutine.lua`. Errors
-- raised with `error` use level 0 or non-string values: Moonseed does not
-- yet add `error`'s position.

local function pack(...) return { n = select("#", ...), ... } end
local function show(t)
  local out = {}
  for i = 1, t.n do
    local v = t[i]
    if type(v) == "table" or type(v) == "function" or type(v) == "thread" then v = type(v) end
    out[#out + 1] = tostring(v)
  end
  return t.n .. ":" .. table.concat(out, ",")
end
local function tb(...)
  return (debug.traceback(...):gsub("\n\t%[C%]: in %?$", ""))
end

-- create, resume, yield: counts and nil holes
local co = coroutine.create(function(...)
  print("A1", show(pack(...)))
  print("A2", show(pack(coroutine.yield())))
  print("A3", show(pack(coroutine.yield(nil))))
  print("A4", show(pack(coroutine.yield(nil, nil))))
  print("A5", show(pack(coroutine.yield(1, nil, 3, nil))))
  return nil, "r", nil
end)
print("A6", show(pack(coroutine.resume(co))))
print("A7", show(pack(coroutine.resume(co, nil))))
print("A8", show(pack(coroutine.resume(co, nil, nil))))
print("A9", show(pack(coroutine.resume(co, "a", nil))))
print("A10", show(pack(coroutine.resume(co))))
print("A11", show(pack(coroutine.resume(co))), coroutine.status(co))
local big = {}
for i = 1, 250 do big[i] = (i % 3 == 0) and nil or i end
local echo = coroutine.create(function(...)
  local got = pack(...)
  while true do got = pack(coroutine.yield(table.unpack(got, 1, got.n))) end
end)
local r = pack(coroutine.resume(echo, table.unpack(big, 1, 250)))
print("A12", r.n, r[1], r[2], r[3], r[250], r[251])
r = pack(coroutine.resume(echo, table.unpack(big, 1, 250)))
print("A13", r.n, r[4], r[249])

-- status, running, isyieldable from every vantage point
local main, ismain = coroutine.running()
print("B1", type(main), ismain, coroutine.status(main), coroutine.isyieldable(), coroutine.isyieldable(main))
local A, B, C
C = coroutine.create(function()
  print("B2", coroutine.status(A), coroutine.status(B), coroutine.status(C), coroutine.status(main))
  print("B3", coroutine.isyieldable(), coroutine.isyieldable(A), coroutine.isyieldable(B), select(2, coroutine.running()))
  coroutine.yield("c")
end)
B = coroutine.create(function()
  print("B4", coroutine.status(A), coroutine.status(B), coroutine.status(C))
  print("B5", show(pack(coroutine.resume(C))))
  print("B6", coroutine.status(C))
  coroutine.yield("b")
end)
A = coroutine.create(function()
  print("B7", show(pack(coroutine.resume(B))))
  print("B8", coroutine.status(B), coroutine.status(C))
  return "a"
end)
print("B9", coroutine.status(A), coroutine.isyieldable(A))
print("B10", show(pack(coroutine.resume(A))))
print("B11", coroutine.status(A), coroutine.status(B), coroutine.status(C), coroutine.isyieldable(A))
print("B12", show(pack(coroutine.resume(main))), show(pack(coroutine.resume(coroutine.running()))))
local self_co
self_co = coroutine.create(function() return coroutine.resume(self_co) end)
print("B13", show(pack(coroutine.resume(self_co))))
local X, Y
X = coroutine.create(function() return coroutine.resume(Y) end)
Y = coroutine.create(function() return coroutine.resume(X) end)
print("B14", show(pack(coroutine.resume(X))))

-- errors
local failing = coroutine.create(function(x) local y = x * 2 coroutine.yield(y) error("boom", 0) end)
print("C1", show(pack(coroutine.resume(failing, 21))))
print("C2", show(pack(coroutine.resume(failing))), coroutine.status(failing))
print("C3", show(pack(coroutine.resume(failing))))
local obj = {}
local tfail = coroutine.create(function() error(obj) end)
local ok, e = coroutine.resume(tfail)
print("C4", ok, e == obj)
print("C5", pcall(coroutine.yield, 1))
print("C6", pcall(coroutine.create, {}))
print("C7", pcall(coroutine.create, setmetatable({}, { __call = print })))
print("C8", pcall(coroutine.resume))
print("C9", pcall(coroutine.resume, {}))
print("C10", pcall(coroutine.status, nil))
print("C11", pcall(coroutine.close))
print("C12", pcall(coroutine.isyieldable, 1))
print("C13", pcall(coroutine.wrap, 1))
print("C14", select("#", coroutine.isyieldable()), pcall(coroutine.isyieldable, nil))

-- native bodies
local p = coroutine.create(print)
print("D1", show(pack(coroutine.resume(p, "printed", nil, 3))), coroutine.status(p))
local er = coroutine.create(error)
print("D2", show(pack(coroutine.resume(er, "e", 0))), coroutine.status(er))
local y = coroutine.create(coroutine.yield)
print("D3", show(pack(coroutine.resume(y, 1, 2))), show(pack(coroutine.resume(y, 3))), coroutine.status(y))
local pc = coroutine.create(pcall)
print("D4", show(pack(coroutine.resume(pc, function(a) local b = coroutine.yield(a + 1) error(b, 0) end, 10))))
print("D5", show(pack(coroutine.resume(pc, "inner"))), coroutine.status(pc))
local sel = coroutine.wrap(select)
print("D6", sel("#", 1, nil, nil), type(sel))

-- close
local function mt(f) return setmetatable({}, { __close = f }) end
local log = {}
local c1 = coroutine.create(function()
  local a <close> = mt(function(_, e) log[#log + 1] = "a:" .. tostring(e) end)
  local b <close> = mt(function(_, e) log[#log + 1] = "b:" .. tostring(e) error("closeerr", 0) end)
  coroutine.yield(1)
end)
coroutine.resume(c1)
print("E1", show(pack(coroutine.close(c1))), table.concat(log, " "), coroutine.status(c1))
print("E2", show(pack(coroutine.close(c1))), show(pack(coroutine.resume(c1))))
local c2 = coroutine.create(function() local a <close> = mt(function(_, e) log[#log + 1] = "c2:" .. tostring(e) end) error("died", 0) end)
print("E3", show(pack(coroutine.resume(c2))))
print("E4", show(pack(coroutine.close(c2))), log[#log])
print("E5", show(pack(coroutine.close(c2))))
print("E6", show(pack(coroutine.close(coroutine.create(print)))))
local fresh = coroutine.create(function() end)
coroutine.close(fresh)
print("E7", coroutine.status(fresh), show(pack(coroutine.resume(fresh))))
print("E8", pcall(coroutine.close, coroutine.running()))
coroutine.wrap(function() print("E9", pcall(coroutine.close, main)) end)()
local self_close
self_close = coroutine.create(function()
  local x <close> = mt(function() print("E10", pcall(coroutine.close, self_close)) end)
  coroutine.yield()
end)
coroutine.resume(self_close)
print("E11", coroutine.close(self_close))
local yclose = coroutine.create(function() local x <close> = mt(function() coroutine.yield() end) coroutine.yield() end)
coroutine.resume(yclose)
print("E12", coroutine.close(yclose))
local okclose = coroutine.create(function() local x <close> = mt(function(_, e) log[#log + 1] = "ok:" .. tostring(e) end) coroutine.yield() end)
coroutine.resume(okclose)
print("E13", show(pack(coroutine.close(okclose))), log[#log])

-- wrap
local w = coroutine.wrap(function(...)
  local a = pack(coroutine.yield(...))
  return "end", a.n
end)
print("F1", show(pack(w(1, nil, 3))))
print("F2", show(pack(w())))
print("F3", pcall(w))
local function callw(f) local r = pack(f()) return r end
local wd = coroutine.wrap(function() error("wrapped", 0) end)
print("F4", pcall(callw, wd))
print("F5", pcall(callw, wd))
local we = coroutine.wrap(function() error(obj) end)
print("F6", select(2, pcall(we)) == obj)
local wc = coroutine.wrap(function()
  local a <close> = mt(function(_, e) log[#log + 1] = "w:" .. tostring(e) error("closing", 0) end)
  error("orig", 0)
end)
print("F7", pcall(callw, wc), log[#log])
local wy = coroutine.wrap(function() local a <close> = mt(function() coroutine.yield() end) error("o", 0) end)
print("F8", pcall(wy))
print("F9", coroutine.wrap(print) ~= coroutine.wrap(print), type(coroutine.wrap(print)))
local gen = coroutine.wrap(function() for i = 1, 3 do coroutine.yield(i, i * i) end end)
for i, sq in gen do print("F10", i, sq) end
local function words(s) return coroutine.wrap(function() for w in s:gmatch("%a+") do coroutine.yield(w) end end) end
local collected = {}
for word in words("one two three") do collected[#collected + 1] = word end
print("F11", table.concat(collected, "+"))
local rec
rec = coroutine.wrap(function() return rec() end)
print("F12", pcall(rec))

-- nesting depth
local depth = 0
local function nest() depth = depth + 1 local c = coroutine.create(nest) local ok2, e2 = coroutine.resume(c) if not ok2 then error(e2, 0) end end
print("G1", pcall(nest))
print("G2", depth)
local function nestw(k) if k == 0 then return "bottom" end local c = coroutine.wrap(nestw) local r2 = c(k - 1) return r2 end
print("G3", pcall(nestw, 150))
print("G4", (select(2, pcall(nestw, 197)):gsub("corpus_coroutine.lua:%d+: ", "")))

-- yields across boundaries
local yieldable = coroutine.wrap(function()
  print("H1", pcall(coroutine.yield, "through pcall"))
  print("H2", pcall(tostring, setmetatable({}, { __tostring = function() coroutine.yield() return "" end })))
  print("H3", pcall(table.sort, { 3, 1, 2 }, function(a2, b2) coroutine.yield() return a2 < b2 end))
  print("H4", pcall(string.gsub, "ab", "a", function() coroutine.yield() end))
  print("H5", pcall(load, function() coroutine.yield() end))
  package.preload.yielder = function() coroutine.yield() end
  print("H6", pcall(require, "yielder"))
  print("H7", xpcall(error, function(m) return select(2, pcall(coroutine.yield)) end, "x"))
  local t = setmetatable({}, { __index = function(_, k) return coroutine.yield(k) end })
  print("H8", t.key)
  local s = setmetatable({}, { __add = function() return coroutine.yield("add") end })
  print("H9", s + 1)
  print("H10", select(2, pcall(function() return coroutine.yield("in pcall") end)))
  print("H11", coroutine.isyieldable())
  return "done"
end)
print("H12", show(pack(yieldable())))
print("H13", show(pack(yieldable("k-val"))))
print("H14", show(pack(yieldable("add-val"))))
print("H15", show(pack(yieldable("pcall-val"))))

-- debug views of coroutines
local dbg = coroutine.create(function(a)
  local b = a + 1
  coroutine.yield(b)
  error("dbg-fail", 0)
end)
print("I1", debug.getinfo(dbg, 0), tb(dbg))
coroutine.resume(dbg, 1)
print("I2", debug.getlocal(dbg, 1, 1), debug.getlocal(dbg, 1, 2))
print("I3", debug.getinfo(dbg, 0, "S").what, debug.getinfo(dbg, 1, "l").currentline)
print("I4", tb(dbg, "susp"))
debug.setlocal(dbg, 1, 2, 99)
coroutine.resume(dbg)
print("I5", coroutine.status(dbg), tb(dbg))
print("I6", select(2, debug.getlocal(dbg, 1, 2)))
coroutine.close(dbg)
print("I7", debug.getinfo(dbg, 0), tb(dbg))
local outer
outer = coroutine.create(function()
  local inner = coroutine.create(function() print("I8", tb(outer)) end)
  coroutine.resume(inner)
end)
coroutine.resume(outer)
local nat = coroutine.create(error)
coroutine.resume(nat, "x")
print("I9", tb(nat), debug.getinfo(nat, 1))
local natp = coroutine.create(coroutine.yield)
coroutine.resume(natp)
print("I10", tb(natp))

-- levels seen by a __close that an unwind or a coroutine close runs
local function levels(tag)
  local out = {}
  for l = 2, 5 do
    local i = debug.getinfo(l, "Sn")
    if not i or i.what == "main" then break end
    out[#out + 1] = i.what .. ":" .. tostring(i.name)
  end
  print(tag, table.concat(out, " "))
end
local function onerr() local x <close> = mt(function() levels("J1") end) error(43) end
print("J2", pcall(onerr))
local function onreturn() local x <close> = mt(function() levels("J3") end) return 1 end
onreturn()
local function outer_fn()
  local y <close> = mt(function() levels("J4") end)
  local function inner_fn() local x <close> = mt(function() levels("J5") end) error("e", 0) end
  inner_fn()
end
print("J6", pcall(outer_fn))
local cj = coroutine.create(function() local x <close> = mt(function() levels("J7") end) coroutine.yield() end)
coroutine.resume(cj) coroutine.close(cj)
local cf = coroutine.create(function() local x <close> = mt(function() levels("J8") end) error("x", 0) end)
coroutine.resume(cf) coroutine.close(cf)
local cp = coroutine.create(function() return pcall(onerr) end)
print("J9", coroutine.resume(cp))
print("done")
