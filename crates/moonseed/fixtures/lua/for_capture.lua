local first
local second

for i = 1, 2 do
    if i == 1 then
        first = function()
            return i
        end
    else
        second = function()
            return i
        end
    end
end

local third

for i = 1, 3 do
    local x = i + 10
    third = function()
        return x
    end
    if i == 2 then
        break
    end
end

return first(), second(), third()
