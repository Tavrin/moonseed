local a, b, c = many()
local x, y = (many())
local t = { many() }
local z = none()
local k = { none() }

return a, b, c, x, y, t[1], t[2], t[3], z, k[1], add(1, 2), many()
