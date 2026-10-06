-- Field reads and writes on a few long-lived tables, string-keyed and array-indexed.
local p = { x = 0, y = 0, vx = 3, vy = -2, hits = 0 }
local a = {}
for i = 1, 256 do a[i] = i end
local sum = 0
for round = 1, 600000 do
  for _ = 1, 10 do
    p.x = p.x + p.vx
    p.y = p.y + p.vy
    if p.x > 1000 or p.x < -1000 then p.vx = -p.vx; p.hits = p.hits + 1 end
    if p.y > 1000 or p.y < -1000 then p.vy = -p.vy; p.hits = p.hits + 1 end
  end
  local k = round % 256 + 1
  a[k] = a[k] + p.hits
  sum = sum + a[k] % 7
end
print("table_fields", p.x, p.y, p.hits, sum)
