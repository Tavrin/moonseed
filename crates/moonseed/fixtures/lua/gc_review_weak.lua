-- Weak/ephemeron/finalizer semantics in generational mode, decided by
-- reachability only (full collections at the points that print).
collectgarbage("generational")
local function count(t) local n = 0 for _ in pairs(t) do n = n + 1 end return n end
local out = {}
local function say(...) out[#out + 1] = table.concat({...}, " ") end

-- 1. Old ephemeron table, young keys some kept, values referring to keys.
local eph = setmetatable({}, {__mode = "k"})
collectgarbage()
local keep = {}
for i = 1, 50 do
  local k = {i}
  eph[k] = {k, i}           -- value refers to its key: still collectable
  if i % 5 == 0 then keep[#keep + 1] = k end
  for j = 1, 20 do local g = {j} end
end
for i = 1, 5 do collectgarbage("step", 0) end
collectgarbage()
say("eph", count(eph))

-- 2. Old strong table whose metatable gains __mode "v" late.
local mt = {}
local late = setmetatable({}, mt)
collectgarbage()
for i = 1, 30 do late[i] = {i} end
collectgarbage("step", 0)
mt.__mode = "v"
for i = 1, 3 do collectgarbage("step", 0) end
collectgarbage()
say("late", count(late))

-- 3. Weak-valued old table, resurrected values (finalizer keeps them).
local wv = setmetatable({}, {__mode = "v"})
local saved = {}
collectgarbage()
for i = 1, 20 do
  local o = setmetatable({i}, {__gc = function(o) saved[#saved + 1] = o end})
  wv[i] = o
end
for i = 1, 4 do collectgarbage("step", 0) end
collectgarbage()
say("resurrected", count(wv), #saved)
saved = nil
collectgarbage()
collectgarbage()

-- 4. Weak keys: an old key in a young ephemeron, a young key in an old one,
-- chains through ephemeron values.
local chain = setmetatable({}, {__mode = "k"})
collectgarbage()
local head = {}
local cur = head
for i = 1, 40 do local nxt = {} chain[cur] = nxt cur = nxt end
for i = 1, 4 do collectgarbage("step", 0) end
collectgarbage()
say("chain kept", count(chain))
head = nil
collectgarbage()
say("chain dropped", count(chain))

-- 5. Re-registration in a finalizer, an old object.
local times = 0
local mt2 = {}
mt2.__gc = function(o) times = times + 1 if times < 3 then setmetatable(o, mt2) end end
do local o = setmetatable({}, mt2) end
collectgarbage()
collectgarbage()
collectgarbage()
collectgarbage()
say("reregistered", times)

-- 6. Strings in weak tables are values, not collected.
local ws = setmetatable({}, {__mode = "kv"})
collectgarbage()
for i = 1, 10 do ws["k" .. i] = "v" .. i end
collectgarbage()
say("strings", count(ws))

-- 7. Coroutine holding young values in its stack while old.
local co = coroutine.create(function()
  local t = {}
  for i = 1, 100 do t[i] = {i} coroutine.yield() end
  local s = 0
  for i = 1, 100 do s = s + t[i][1] end
  return s
end)
collectgarbage()
for i = 1, 100 do coroutine.resume(co) for j = 1, 30 do local g = {j} end if i % 10 == 0 then collectgarbage("step", 0) end end
say("co", select(2, coroutine.resume(co)))

print(table.concat(out, "\n"))
