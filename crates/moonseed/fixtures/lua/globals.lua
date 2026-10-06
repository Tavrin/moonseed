x = 40
x = x + 2

f = function()
    return 42
end

local g = function()
    y = 7
    return y
end

local r = g()

return x, f(), r, y, missing
