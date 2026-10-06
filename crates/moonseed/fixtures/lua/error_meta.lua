local mt = {
    __index = function() error("idx", 0) end,
    __newindex = function() error("nidx", 0) end,
    __len = function() error("len", 0) end,
    __call = function() error("call", 0) end,
    __add = function() error("add", 0) end,
    __eq = function() error("eq", 0) end,
    __lt = function() error("lt", 0) end,
    __concat = function() error("cat", 0) end,
    __unm = function() error("unm", 0) end
}
local t = setmetatable({}, mt)
local u = setmetatable({}, mt)
local try = function(f)
    local ok, e = pcall(f)
    return e
end
local stored = try(function() t.y = 1 end)
local native = setmetatable({}, { __add = add })

return try(function() return t.x end), stored, rawget(t, "y"), try(function() return #t end), try(function() return t() end),
    try(function() return t + 1 end), try(function() return t == u end), try(function() return t < u end),
    try(function() return t .. "x" end), try(function() return -t end), (pcall(function() return native + 1 end))
