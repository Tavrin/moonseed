local ok1, ok2, err = pcall(function()
    return pcall(function()
        error("inner", 0)
    end)
end)
local outer, msg = pcall(function()
    local ok, e = pcall(function()
        error("a", 0)
    end)
    error(e .. "b", 0)
end)
local deep = function(n, f)
    return f(n)
end
local okd, errd = pcall(deep, 5, function(n)
    return pcall(error, n * 2, 0)
end)

return ok1, ok2, err, outer, msg, okd, errd
