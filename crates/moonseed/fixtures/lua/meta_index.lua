local defaults = { x = 42 }
local t = {}
setmetatable(t, { __index = defaults })

local f = setmetatable({}, {
    __index = function(self, key)
        return key
    end
})

local chain_c = { deep = 7 }
local chain_b = setmetatable({}, { __index = chain_c })
local chain_a = setmetatable({}, { __index = chain_b })

local hit = setmetatable({ x = 1 }, {
    __index = function()
        return 99
    end
})

local many = setmetatable({}, {
    __index = function()
        return 5, 6
    end
})

local none = setmetatable({}, {
    __index = function()
    end
})

local native = setmetatable({}, { __index = second })

return t.x, f.answer, chain_a.deep, hit.x, many.y, none.z, t.missing, rawget(t, "x"), native.key
