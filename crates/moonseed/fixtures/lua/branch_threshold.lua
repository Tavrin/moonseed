local inner_get

local outer = 1
local outer_get = function()
    return outer
end

if true then
    local inner = 2
    inner_get = function()
        return inner
    end
else
    inner_get = function()
        return 0
    end
end

outer = 3

return outer_get(), inner_get()
