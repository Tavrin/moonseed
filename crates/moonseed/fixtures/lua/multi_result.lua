-- Lua 5.4 multi-result adjustment.
-- select('#', count(many())) == 4
-- select('#', count((many()))) == 2
-- count(many()) itself is 3, 10, nil, 30
-- count((many())) itself is 1, 10
-- many() adjusted to four slots is 10, nil, 30, nil
-- many() adjusted to two slots is 10, nil
-- a zero-result call padded to two locals yields nil, nil
-- a one-result call yields 7

local function many()
    return 10, nil, 30
end

local function count(...)
    return select('#', ...), ...
end

local function none()
    return
end

local function one()
    return 7
end

local open_n = select('#', count(many()))
local paren_n = select('#', count((many())))
local o1, o2, o3, o4 = count(many())
local p1, p2 = count((many()))
local a, b, c, d = many()
local t1, t2 = many()
local single = one()
local z1, z2 = none()

assert(open_n == 4)
assert(paren_n == 2)
assert(o1 == 3 and o2 == 10 and o3 == nil and o4 == 30)
assert(p1 == 1 and p2 == 10)
assert(a == 10 and b == nil and c == 30 and d == nil)
assert(t1 == 10 and t2 == nil)
assert(single == 7)
assert(z1 == nil and z2 == nil)

print(string.format(
    "%d %d %d %s %d %d %d %d %s %d %s %d %s %d",
    open_n,
    paren_n,
    o1,
    tostring(o3),
    o4,
    p1,
    p2,
    a,
    tostring(b),
    c,
    tostring(d),
    t1,
    tostring(t2),
    single
))
