-- Numeric for loops: integer and float accumulation, nested, with a step.
local isum = 0
for i = 1, 60000000 do
  isum = isum + (i & 255)
end
local fsum = 0.0
for i = 1, 10000000 do
  fsum = fsum + i * 0.5
end
local n = 0
for i = 1, 2000 do
  for j = i, 2000, 3 do
    n = n + 1
  end
end
print("numeric_loops", isum, string.format("%.1f", fsum), n)
