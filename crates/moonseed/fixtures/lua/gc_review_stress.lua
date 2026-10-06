-- Review stress: old structures receiving young values through paths
-- other than plain table stores. Output depends only on reachability.
collectgarbage("generational", 5, 50)
local old = {}
for i = 1, 40 do old[i] = {i} end
local oldf = {}
for i = 1, 10 do
  local cap = {i}
  oldf[i] = function(v) if v then cap = v end return cap end
end
local function holder(i) local u = {i} return function(v) if v ~= nil then u = v end return u end end
local hold = {}
for i = 1, 10 do hold[i] = holder(i) end
local co = coroutine.create(function(a)
  local acc = {a}
  while true do
    local got = coroutine.yield(acc)
    acc = {got, acc[1]}
  end
end)
local co2 = coroutine.wrap(function()
  local list = {}
  local function push(v) list[#list + 1] = v end
  while true do
    local v = coroutine.yield(push, list)
    if #list > 20 then list = {list[#list]} end
  end
end)
local strmeta = {}
debug.setmetatable(true, nil)
local errs = {}
local sorted = {}
local mover = {}
local envs = {}
local ttl = {}
collectgarbage()
collectgarbage()
local checksum = 0
for round = 1, 600 do
  local young = {r = round}
  -- debug.setupvalue on an old closure
  debug.setupvalue(oldf[round % 10 + 1], 1, {round})
  -- upvaluejoin: old closure joined to a young closure's upvalue
  local yh = holder(round)
  debug.upvaluejoin(hold[round % 10 + 1], 1, yh, 1)
  -- old coroutine receives young values
  local ok, acc = coroutine.resume(co, {round})
  old[round % 40 + 1][2] = acc
  -- coroutine.wrap with closures over its locals
  local push, list = co2()
  push({round})
  -- table library on old tables
  table.insert(sorted, {round % 17, tostring(round)})
  if #sorted > 30 then table.remove(sorted, 1) end
  table.sort(sorted, function(a, b) if a[1] ~= b[1] then return a[1] < b[1] end return a[2] < b[2] end)
  table.move({{round}, {round + 1}}, 1, 2, round % 5 + 1, mover)
  -- errors caught into an old table
  local ok2, e = pcall(error, {round})
  errs[round % 7 + 1] = e
  -- string keys made now, in an old table
  ttl[("k"):rep(2) .. round % 13] = {round}
  -- gsub building new strings into an old table
  old[round % 40 + 1][3] = string.gsub("a-b-c", "%a", function(c) return c .. round end)
  -- load with a new environment kept by an old table
  local env = {x = {round}}
  envs[round % 3 + 1] = load("return x", "c", "t", env)
  -- type metatable holding a new table
  strmeta.__index = {len = round}
  debug.setmetatable(true, strmeta)
  -- to-be-closed variable
  do
    local c <close> = setmetatable({}, {__close = function() old[1][4] = {round} end})
  end
  -- garbage
  for j = 1, 8 do local g = {j, {j}} end
  if round % 50 == 0 then collectgarbage("step", 0) end
  if round % 173 == 0 then collectgarbage() end
  if round % 211 == 0 then collectgarbage("incremental") collectgarbage("generational", 5, 50) end
end
collectgarbage()
for i = 1, 10 do checksum = checksum + oldf[i]()[1] + hold[i]()[1] end
for i = 1, 40 do
  local a = old[i][2]
  checksum = checksum + a[1][1] + #old[i][3]
end
local _, l = co2()
checksum = checksum + #l + l[#l][1]
for i = 1, #sorted do checksum = checksum + sorted[i][1] end
for i = 1, 7 do checksum = checksum + errs[i][1] end
for k, v in pairs(ttl) do checksum = checksum + v[1] end
for i = 1, 3 do checksum = checksum + envs[i]()[1] end
for i = 1, 6 do checksum = checksum + mover[i][1] end
checksum = checksum + (true).len + old[1][4][1]
print(checksum)
