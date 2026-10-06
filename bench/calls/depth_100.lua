-- Recursive call-window locality probe; change only the iteration count for slopes.
local iterations = 200 -- DEPTH_ITERATIONS
local function descend(n)
  if n == 0 then return 0 end
  return 1 + descend(n - 1)
end
for _ = 1, 10 do assert(descend(100) == 100) end
local sum = 0
for _ = 1, iterations do sum = sum + descend(100) end
assert(sum == iterations * 100)
print(sum)
