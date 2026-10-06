local t = {}
for i = 1, 256 do t[i] = i end
local sum = 0
for i = 1, 15000000 do
  local k = i % 256 + 1
  t[k] = t[k] + 1
  sum = sum + t[k]
end
print("array_access", sum)
