local inc
local get

if true then
    local x = 10

    inc = function()
        x = x + 1
        return x
    end

    get = function()
        return x
    end
else
    inc = function()
        return 0
    end

    get = function()
        return 0
    end
end

local x = 99

return get(), inc(), get(), x
