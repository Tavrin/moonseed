local t = {}
t[1] = 10
t["x"] = 20

local key = "x"
local n = { nested = { value = 42 } }
local d = { a = 1 }
d.a = nil
local g = { x = 12 }

local c = (function()
    return { k = 5 }
end)().k

local m = ({})["missing"]
t.y = t.x
t[t[1]] = "ten"

return t[1], t.x, n.nested.value, d.a, g[key], c, m, t.y, t[10] == "ten"
