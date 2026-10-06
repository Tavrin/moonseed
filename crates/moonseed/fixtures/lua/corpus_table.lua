-- A generated corpus of table calls (ADR 0033): each function over lists
-- with metamethods and edge positions, and sorts of every size to 40.
-- Compared with Lua 5.4.9.
local function dump(t)
  local out = {}
  for i = -1, 7 do out[#out + 1] = tostring(rawget(t, i)) end
  return table.concat(out, ",")
end
local function lists()
  return {
    {},
    { 1, 2, 3 },
    -- (No list with a hole: its length is any border, and Moonseed and
    -- PUC pick different ones, both legal.)
    { [1] = 1, [3] = 3, [10] = 10 },
    { "a", "b", "c", "d", "e" },
    setmetatable({ 1, 2, 3 }, { __len = function() return 5 end }),
    setmetatable({}, { __index = function(t, k) return k * 2 end, __len = function() return 4 end }),
    setmetatable({ 1, 2 }, { __newindex = function(t, k, v) rawset(t, k, v and v * 10 or v) end }),
  }
end
local positions = { nil, 0, 1, 2, 3, 4, 5, 6, -1, 10, math.maxinteger, math.mininteger }
local case = 0
local function run(label, f)
  case = case + 1
  for li, list in ipairs(lists()) do
    local ok, a, b = pcall(f, list)
    print(case, label, li, ok, ok and tostring(a) or "error", ok and tostring(b) or "", dump(list))
  end
end
for pi = 1, 12 do -- `positions` starts with nil
  local p = positions[pi]
  run("insert" .. pi, function(t) if p == nil then return table.insert(t, "v") end return table.insert(t, p, "v") end)
  run("remove" .. pi, function(t) if p == nil then return table.remove(t) end return table.remove(t, p) end)
  run("unpack" .. pi, function(t) if p == nil then return select("#", table.unpack(t)) end return select("#", table.unpack(t, p, 3)), (table.unpack(t, p, 3)) end)
  run("concat" .. pi, function(t) if p == nil then return table.concat(t, "|") end return table.concat(t, "|", p, 3) end)
end
for _, spec in ipairs({ {1, 3, 2}, {2, 4, 1}, {1, 3, 3}, {1, 0, 1}, {3, 1, 1}, {1, 5, 1}, {0, 2, 4}, {-1, 1, 2}, {1, 2, 6} }) do
  run("move" .. spec[1] .. spec[2] .. spec[3], function(t) return table.move(t, spec[1], spec[2], spec[3]) == t end)
  run("move2-" .. spec[1] .. spec[2] .. spec[3], function(t) local d = table.move(t, spec[1], spec[2], spec[3], {}) return dump(d) end)
end
run("sort", function(t) table.sort(t, function(a, b) return (tostring(a)) < (tostring(b)) end) end)
run("sortdefault", function(t) table.sort(t) end)
run("sortdesc", function(t) table.sort(t, function(a, b) return (a or 0) > (b or 0) end) end)
for n = 0, 40 do
  local t = {}
  for i = 1, n do t[i] = (i * 37 + n) % 11 end
  local calls = 0
  table.sort(t, function(a, b) calls = calls + 1 return a < b end)
  print("sortn", n, calls, table.concat(t, ","))
  local u = {}
  for i = 1, n do u[i] = (i * 13) % 7 end
  local ok = pcall(table.sort, u, function(a, b) return a <= b end)
  print("sortle", n, ok, table.concat(u, ","))
end
for _, n in ipairs({ 101, 150, 300 }) do
  local t = {}
  for i = 1, n do t[i] = (i * 7919) % 97 end
  local calls = 0
  table.sort(t, function(a, b) calls = calls + 1 return a < b end)
  print("sortbig", n, calls, t[1], t[n // 2], t[n])
end
return "done"
