local f
local i = 0

while true do
    do
        local x = 42
        f = function()
            return x
        end

        break
    end

    i = 999
end

local x = 77

return f(), x, i
