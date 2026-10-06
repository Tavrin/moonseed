local f

do
    local x = 10
    f = function()
        return x
    end
end

local x = 99

return f(), x
