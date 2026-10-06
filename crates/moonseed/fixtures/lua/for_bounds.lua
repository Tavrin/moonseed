local a = 0
local last_a
for i = 9223372036854775805, 9223372036854775807 do
    a = a + 1
    last_a = i
end

local b = 0
local last_b
for i = -9223372036854775806, 0x8000000000000000, -1 do
    b = b + 1
    last_b = i
end

local c = 0
local last_c
for i = 1, 9223372036854775807, 0x4000000000000000 do
    c = c + 1
    last_c = i
end

local d = 0
for i = 0, -1, 0x8000000000000000 do
    d = d + 1
end

local e = 0
local last_e
for i = 0, 0x8000000000000000, 0x8000000000000000 do
    e = e + 1
    last_e = i
end

local f = 0
for i = 1, 1e300 do
    f = f + 1
    if f == 3 then
        break
    end
end

local g = 0
for i = 1, -1e300 do
    g = g + 1
end

local h = 0
for i = -1, -1e300, -1 do
    h = h + 1
    if h == 2 then
        break
    end
end

local j = 0
for i = 1, 1e300, -1 do
    j = j + 1
end

local k = 0
for i = 9007199254740992, 9007199254740993.0 do
    k = k + 1
end

local l = 0
for i = 9007199254740991, 9007199254740993 do
    l = l + 1
end

return a, last_a, b, last_b, c, last_c, d, e, last_e, f, g, h, j, k, l
