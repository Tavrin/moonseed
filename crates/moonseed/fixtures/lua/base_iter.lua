-- `next`, `pairs`, and `ipairs` (ADR 0031). Traversal order is left to
-- the implementation, so only order-free facts are printed.
print((pcall(next)))
print((pcall(next, 1)))
print(select("#", next({})), next({}), next({}, nil))
print(pcall(next, {}, "missing"))
local t = { 10, 20, x = 1 }
print(pcall(next, t, 3))
local count, sum = 0, 0
local k, v = next(t)
while k ~= nil do
  count = count + 1
  sum = sum + v
  k, v = next(t, k)
end
print(count, sum)
local f, s, c = pairs(t)
print(f == next, s == t, c, select("#", pairs(t)))
print((pcall(pairs)))
print(select("#", pairs(1)), select(2, pairs(1)))
print(pairs(setmetatable({}, { __pairs = function() return 1, 2, 3, 4 end })))
print(select("#", pairs(setmetatable({}, { __pairs = function() return 1 end }))))
print((pcall(pairs, setmetatable({}, { __pairs = 5 }))))
print(pairs(setmetatable({}, { __pairs = setmetatable({}, { __call = function(self, t) return "c" end }) })))
local mt = { __pairs = function(t) return function(_, k) if not k then return 1, "one" end end, t, nil end }
for k, v in pairs(setmetatable({}, mt)) do print(k, v) end
-- Clearing each field while traversing, as Lua allows.
local d = { 1, 2, 3, 4, 5, a = 1, b = 2 }
local cleared = 0
for k in pairs(d) do
  d[k] = nil
  cleared = cleared + 1
end
print(cleared, next(d))
local sq = 0
for key, value in pairs({ 1, 2, 3, 4 }) do sq = sq + key * value end
print(sq)
print((pcall(ipairs)))
local g, s2, z = ipairs(t)
print(s2 == t, z, select("#", ipairs(t)), g == ipairs({}))
print(g(t, 0), g(t, 1), select("#", g(t, 2)), g(t, 2))
print((pcall(g, nil, 0)))
print(pcall(g, t, "1"))
print(pcall(g, t, 1.0))
print((pcall(g, t, 1.5)))
print(pcall(g, t, 9223372036854775807))
print((pcall(g, t)))
local p = setmetatable({}, { __index = function(t, i) if i < 4 then return i * 2 end end })
for i, v in ipairs(p) do print(i, v) end
local via = setmetatable({}, { __index = { "a", "b" } })
for i, v in ipairs(via) do print(i, v) end
for i in ipairs({ 1, 2, nil, 4 }) do print(i) end
print((pcall(function() for i, v in ipairs(5) do end end)))
local grow = { 1 }
local seen = 0
for i, v in ipairs(grow) do
  seen = seen + 1
  if i < 5 then grow[i + 1] = v + 1 end
end
print(seen, grow[5])
return "done"
