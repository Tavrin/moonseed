x = 1

local outer = function()
    return x
end

local inner

do
    local _ENV = { x = 2 }

    inner = function()
        return x
    end

    x = 3
end

return outer(), inner(), x
