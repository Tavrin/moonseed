local calls = 0

local init = function()
    calls = calls + 1
    if calls == 1 then
        return 1
    end
    return 50
end

local limit = function()
    calls = calls + 1
    if calls == 2 then
        return 4
    end
    return 0
end

local step = function()
    calls = calls + 1
    if calls == 3 then
        return 1
    end
    return 7
end

local n = 0
for i = init(), limit(), step() do
    n = n + 1
end

local after_first = calls

local m = 0
for i = init(), limit() do
    m = m + 1
end

return n, after_first, m, calls
