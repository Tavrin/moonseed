local n = 0

repeat
    local done = n >= 3
    n = n + 1
until done

local i = 0
local last

repeat
    local x = i
    last = function()
        return x
    end
    i = i + 1
until x >= 2

return n, last(), i
