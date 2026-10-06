local i = 0
local first
local second

while i < 2 do
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
end

return first(), second(), i
