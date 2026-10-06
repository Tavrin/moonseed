local upto3 = function(s, c)
  if c == nil then return 1 end
  if c < s then return c + 1 end
end
local sum = 0
for i in upto3, 3 do sum = sum + i end
local pairs2 = function(s, c)
  if c == nil then return 1, 10, 100 end
  if c == 1 then return 2, nil, 200 end
end
local a2, b2, holes = 0, 0, 0
for i, v in pairs2 do
  a2 = a2 + i
  if v == nil then holes = holes + 1 else b2 = b2 + v end
end
local calls = 0
local falsy = function(s, c)
  calls = calls + 1
  if calls == 1 then return false end
  if calls == 2 then return 0, c end
  return nil
end
local seen = 0
local x1, y1, y2
for x, y in falsy do
  seen = seen + 1
  if seen == 1 then x1 = x y1 = y else y2 = y end
end
local empty = 0
for x in none do empty = empty + 1 end
local ctl = function(s, c)
  if c == nil then return 1 end
  if c == 1 then return 2 end
  return nil
end
local total = 0
for x in ctl do total = total + x x = 999 end
local k = 3
local count = 0
for k in upto3, k do count = count + k end
local shape = function(s, c)
  if c == nil then return 10 end
  if c == 10 then return 20, 21 end
  if c == 20 then return 30, 31, 32, 33 end
  if c == 30 then return 40, nil, 42 end
end
local log = {}
local n = 0
for a, b, c in shape do
  n = n + 1 log[n] = a
  n = n + 1 log[n] = b
  n = n + 1 log[n] = c
end
return sum, a2, b2, holes, seen, x1, y1, y2, calls, empty, total, k, count, n, log[1], log[2], log[3], log[4], log[5], log[6], log[7], log[8], log[9], log[10], log[11], log[12]
