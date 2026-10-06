local n = 0
local last
for i = 1.0, 3 do
    n = n + 1
    last = i
end

local m = 0
local last2
for i = 1, 3, 1.0 do
    m = m + 1
    last2 = i
end

local k = 0
for i = 1, 2, 0.25 do
    k = k + 1
end

local r = 0
for i = 3.5, 1, -0.5 do
    r = r + 1
end

return n, last, m, last2, k, r
