local a = 0
for i = 1, 5 do
    a = a + i
end

local b = 0
for i = 1, 10, 3 do
    b = b + i
end

local c = 0
for i = 10, 1, -2 do
    c = c + i
end

local d = 0
for i = 5, 1 do
    d = d + 1
end

local e = 0
for i = 1, 3.9 do
    e = e + i
end

local f = 0
for i = 3, 0.5, -1 do
    f = f + i
end

return a, b, c, d, e, f
