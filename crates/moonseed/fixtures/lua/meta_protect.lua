local mt = { __metatable = "locked" }
local t = setmetatable({}, mt)
local plain_mt = {}
local p = setmetatable({}, plain_mt)
local removed = setmetatable(setmetatable({}, plain_mt), nil)

return getmetatable(t), rawget(mt, "__metatable"), getmetatable(p) == plain_mt,
    getmetatable({}), getmetatable(1), getmetatable(removed), setmetatable(p, nil) == p
