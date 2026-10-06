-- Commutative checksums avoid depending on pairs' unspecified iteration order.
local array = {}
local fields = {}
for i = 1, 128 do
  array[i] = i * 3
  fields["key" .. i] = i * 5
end
local sum = 0
local count = 0
for _ = 1, 40000 do
  for i, value in ipairs(array) do sum = sum + i + value end
  for key, value in pairs(fields) do
    sum = sum + #key + value
    count = count + 1
  end
end
print("generic_for", sum, count)
