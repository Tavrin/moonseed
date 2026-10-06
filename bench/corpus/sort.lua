-- Four pairs of 100000-number sorts: default order and a Lua comparator.
local seed = 12345
local function rnd()
  seed = (seed * 1103515245 + 12345) % 2147483648
  return seed
end
local sum = 0
for _ = 1, 4 do
  local t, u = {}, {}
  for i = 1, 100000 do t[i] = rnd(); u[i] = rnd() end
  table.sort(t)
  table.sort(u, function(a, b) return a > b end)
  for i = 1, 100000 do
    if i > 1 then
      assert(t[i - 1] <= t[i])
      assert(u[i - 1] >= u[i])
    end
    sum = sum + t[i] * (i % 97 + 1) + u[i] * (i % 89 + 1)
  end
end
print("sort", sum)
