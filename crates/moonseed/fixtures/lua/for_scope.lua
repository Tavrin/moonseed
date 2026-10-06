local i = 5
local n = 0

for i = i, i + 1 do
    n = n + i
end

local sum = 0

for j = 1, 3 do
    sum = sum + j
    j = 99
end

return i, n, sum
