local mt = {
    __index = function()
        return 1
    end
}
local t = setmetatable({}, mt)
local u = setmetatable({}, mt)
local a = t.x

mt.__index = function()
    return 2
end

local b, c = t.x, u.x
setmetatable(t, nil)

return a, b, c, t.x, getmetatable(u) == mt
