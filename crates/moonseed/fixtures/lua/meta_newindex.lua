local seen = {}
local t = setmetatable({}, {
    __newindex = function(self, key, value)
        rawset(seen, key, value)
    end
})
t.x = 42

local target = {}
local proxy = setmetatable({}, { __newindex = target })
proxy.y = 7

local existing = setmetatable({ z = 1 }, {
    __newindex = function()
        never()
    end
})
existing.z = 2

local count = 0
local log = {}
local u = setmetatable({}, {
    __newindex = function(self, key, value)
        count = count + 1
        rawset(log, key, value)
    end
})
local i = 1
i, u[i] = 2, 99

return rawget(t, "x"), seen.x, target.y, rawget(proxy, "y"), existing.z, i, log[1], log[2], count
