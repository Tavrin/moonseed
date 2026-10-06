local f
local k = 2

if k == 1 then
    f = function()
        return 1
    end
elseif k == 2 then
    local x = 20
    f = function()
        return x
    end
elseif k == 3 then
    f = function()
        return 3
    end
else
    f = function()
        return 4
    end
end

local x = 99

return f(), x
