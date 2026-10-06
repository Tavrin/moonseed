local f
f = function()
    return f() + 1
end
local ok = pcall(f)
local a = {}
local b = setmetatable({}, { __eq = function(x, y) return x == a end })
local ok2 = pcall(function() return b == a end)
local c = {}
setmetatable(c, { __call = function(self) return (c()) end })
local ok3 = pcall(c)
local ok4, e4 = pcall(error, "m", 0)
local ok5 = pcall(error, "m", "x")

return ok, ok2, ok3, ok4, e4, ok5, 1 + 1
