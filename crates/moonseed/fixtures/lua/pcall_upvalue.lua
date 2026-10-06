local saved
local ok, err = pcall(function()
    local x = 10
    saved = function()
        return x
    end
    error("stop", 0)
end)
local get, set
pcall(function()
    local y = 1
    get = function()
        return y
    end
    set = function(v)
        y = v
    end
    error("shared", 0)
end)
local clobber = function()
    local a, b, c, d = 101, 102, 103, 104
    return a + b + c + d
end
local sum = clobber()
set(5)
local x = 99

return ok, err, saved(), get(), x, sum
