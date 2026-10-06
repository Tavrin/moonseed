local i = 0
local first
local second

repeat
    local x = i

    if i == 0 then
        first = function()
            return x
        end
    else
        second = function()
            return x
        end
    end

    i = i + 1
until i >= 2

return first(), second(), i
