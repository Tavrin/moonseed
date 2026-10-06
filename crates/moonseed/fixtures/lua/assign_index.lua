local i = 1
local t = {}
i, t[i] = 2, 99

local a = {}
local j = 1
a[j], j = 5, 7

local calls = 0
local pick = function()
    calls = calls + 1
    return a
end
pick()[j], pick().z = 1, 2

local b = {}
b.x, b.y = 1, 2

return i, t[1], t[2], a[1], j, a[7], a.z, calls, b.x, b.y
