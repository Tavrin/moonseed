local t = { "a", x = 10, "b", [100] = 20, "c" }

local many = function()
    return 10, nil, 30
end

local u = { 1, many() }
local v = { 1, (many()) }
local w = { many(), 2 }
local e = {}
local s = { 1, 2; 3, }

return t[1] == "a", t[2] == "b", t[3] == "c", t.x, t[100],
    u[1], u[2], u[3], u[4], v[2], v[3], w[1], w[2], w[3], e[1], s[3]
