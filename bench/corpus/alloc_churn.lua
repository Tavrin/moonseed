-- Short-lived small tables with a small survivor ring, so collection runs repeatedly.
local ring = {}
for i = 1, 64 do ring[i] = false end
local sum = 0
for i = 1, 3000000 do
  local t = { i, i + 1, x = i, y = i * 2 }
  sum = sum + t[1] + t.y
  if i % 16 == 0 then ring[i % 64 + 1] = t end
end
local kept = 0
for i = 1, 64 do
  if ring[i] then kept = kept + ring[i].x end
end
print("alloc_churn", sum, kept)
