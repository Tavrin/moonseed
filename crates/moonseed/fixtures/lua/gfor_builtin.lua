-- Native iterator result windows, fallbacks, and mutation between calls.
local t = {10, false, nil, 40}
local g = ipairs(t)
assert(select("#", g(t, 0)) == 2)
assert(select("#", g(t, 2)) == 1)
assert(g(t, "0") == 1 and g(t, 0.0) == 1)
assert(not pcall(g, t, {}))
local n = 0
for i, v, extra in ipairs(t) do
  assert(extra == nil and (v == 10 or v == false))
  n = n + i
end
assert(n == 3)
n = 0
for i in ipairs(t) do n = n + i end
assert(n == 3)

local proxy = setmetatable({}, {__index = function(_, i)
  if i <= 3 then return i * 2 end
end})
n = 0
for i, v in ipairs(proxy) do n = n + v end
assert(n == 12)
local grow = {2, 4, 6}
n = 0
for i, v in ipairs(grow) do
  grow[i] = nil
  if i == 3 then grow[4] = 8 end
  n = n + v
end
assert(n == 20)

local fields = {one = 1, two = 2, three = 3}
n = 0
for k, v in pairs(fields) do fields[k] = nil; n = n + v end
assert(n == 6 and next(fields) == nil)
assert(select("#", next(fields)) == 1)
local ok, err = pcall(next, fields, "absent")
assert(not ok and err == "invalid key to 'next'")
assert(not pcall(next, fields, 0/0))
local array = {10, 20}
local k, v, extra = next(array, 1)
assert(k == 2 and v == 20 and extra == nil)
next(array) -- zero-result window
local tail = function(...) return next(...) end
assert(select("#", tail(array, 2)) == 1)
local custom = setmetatable({}, {__pairs = function() return ipairs({3, 7}) end})
n = 0
for k, v in pairs(custom) do n = n + v end
assert(n == 10)
return "iterator windows ok"
