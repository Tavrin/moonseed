-- collectgarbage's incremental surface (Phase 3.28), as Lua 5.4.9
-- answers it. Collection timing is not compared, only what Lua defines:
-- option results, parameter storage, errors, and what a full collection
-- or a step that completes a cycle leaves.

-- A: modes and parameters. Lua stores the pause and the step multiplier
-- divided by four in a byte, and `incremental` leaves a 0 alone.
-- (The standalone `lua` starts in generational mode; Moonseed has only
-- the incremental one.)
collectgarbage("incremental")
print("A", collectgarbage("isrunning"), collectgarbage("incremental"))
print("A", collectgarbage("setpause", 150), collectgarbage("setpause"))
print("A", collectgarbage("setpause", 200), collectgarbage("setpause", 200))
print("A", collectgarbage("setstepmul", 300), collectgarbage("setstepmul", 100))
print("A", collectgarbage("incremental", 0, 0, 0), collectgarbage("setpause", 200))
print("A", collectgarbage("incremental", 180, 0, 0), collectgarbage("setpause", 200))
print("A", collectgarbage("incremental", 0, 404, 14), collectgarbage("setstepmul", 100))
print("A", collectgarbage("setpause", 1023), collectgarbage("setpause", 200))
print("A", collectgarbage("setpause", 1024), collectgarbage("setpause", 200))
print("A", collectgarbage("setpause", -4), collectgarbage("setpause", 200))
print("A", collectgarbage("setstepmul", 2.0), collectgarbage("setstepmul", 100))
print("A", collectgarbage("setpause", 3), collectgarbage("setpause", 200))
print("A", collectgarbage("incremental", 200, 100, 13))

-- B: argument errors.
print("B", pcall(collectgarbage, "setpause", 1.5))
print("B", pcall(collectgarbage, "incremental", "x"))
print("B", pcall(collectgarbage, "incremental", 0, {}))
print("B", pcall(collectgarbage, "step", "big"))
print("B", pcall(collectgarbage, "setstepmul", "7"))
print("B", pcall(collectgarbage, "nope"))

-- C: stop and restart; a step runs while stopped and leaves it stopped.
collectgarbage("stop")
print("C", collectgarbage("isrunning"))
print("C", type(collectgarbage("step", 0)), collectgarbage("isrunning"))
local garbage = {}
for i = 1, 2000 do garbage[i] = {i} end
garbage = nil
print("C", collectgarbage("isrunning"))
print("C", collectgarbage("restart"), collectgarbage("isrunning"))

-- D: a step this large completes a cycle; one that adds nothing does not
-- step; a full collection returns 0.
collectgarbage()
print("D", collectgarbage("step", 20000), collectgarbage("step", 20000))
print("D", collectgarbage("step", -1))
print("D", collectgarbage("collect"), collectgarbage())
print("D", math.type(collectgarbage("count")))

-- E: what a completed cycle leaves, reached by steps: weak values of dead
-- objects cleared, ephemerons settled, finalizers run once, newest
-- registration first.
local weak = setmetatable({}, {__mode = "v"})
local keys = setmetatable({}, {__mode = "k"})
local order = {}
do
  local kept = {}
  weak[1], weak[2] = {}, kept
  keys[kept] = "kept"
  keys[{}] = "dead"
  for i = 1, 3 do
    setmetatable({}, {__gc = function() order[#order + 1] = i end})
  end
  _G.kept = kept
end
repeat until collectgarbage("step", 20000)
local count = 0
for _ in pairs(keys) do count = count + 1 end
print("E", weak[1], weak[2] == kept, count, keys[kept])
print("E", table.concat(order, ","))
_G.kept = nil

-- F: inside a finalizer every option fails, and changes nothing.
setmetatable({}, {__gc = function()
  print("F", collectgarbage("step"), collectgarbage("incremental"),
    collectgarbage("setpause", 100), collectgarbage("setstepmul", 100),
    collectgarbage("isrunning"), collectgarbage("count"))
end})
collectgarbage()
print("F", collectgarbage("setpause", 200), collectgarbage("setstepmul", 100))

-- G: tiny steps (a 64-byte step size) through many cycles while the
-- program builds, links, and drops objects: what is reachable stays, and
-- weak entries go only when their objects do.
collectgarbage("incremental", 200, 100, 6)
local live = {}
local cache = setmetatable({}, {__mode = "v"})
for round = 1, 300 do
  local node = {round = round, next = live[#live]}
  live[#live + 1] = node
  cache[round] = node
  if round % 3 == 0 then
    live[#live] = nil
  end
  -- Which steps end a cycle is the collector's timing, not compared.
  collectgarbage("step", 0)
end
collectgarbage()
local alive, cached = 0, 0
for _, node in ipairs(live) do
  local n = node
  while n do
    alive = alive + 1
    n = n.next
  end
end
for _ in pairs(cache) do cached = cached + 1 end
print("G", #live, cached, alive)
collectgarbage("incremental", 200, 100, 13)
print("end")
