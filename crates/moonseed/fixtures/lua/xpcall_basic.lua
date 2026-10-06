local ok, err = xpcall(function()
    error("a", 0)
end, function(m)
    return "handled " .. m
end)
local ok2, r1, r2 = xpcall(function(a, b)
    return a + b, b
end, function(m)
    return m
end, 20, 22)
local ok3, e3 = xpcall(function()
    error("a", 0)
end, function(m)
    if m == "a" then
        error("b", 0)
    end
    return "final " .. m
end)
local ok4, e4 = xpcall(function()
    error("x", 0)
end, function(m)
    error(m, 0)
end)
local ok5, e5 = xpcall(function()
    error("x", 0)
end, function() end)
local ok6, e6 = xpcall(function()
    error("x", 0)
end, function(m)
    return m, "extra"
end)
local ok7, e7 = pcall(xpcall, error, setmetatable({}, { __call = function() return "t" end }))

return ok, err, ok2, r1, r2, ok3, e3, ok4, e4, ok5, e5, ok6, e6, ok7
