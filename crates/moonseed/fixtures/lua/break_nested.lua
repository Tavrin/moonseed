local outer = 0
local inner_total = 0

while outer < 3 do
    local j = 0
    while true do
        if j >= 2 then
            break
        end
        j = j + 1
        inner_total = inner_total + 1
    end
    outer = outer + 1
end

return outer, inner_total
