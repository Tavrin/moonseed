local make_pair = function()
    local n = 0

    local inc = function()
        n = n + 1
        return n
    end

    local get = function()
        return n
    end

    return inc, get
end

local inc, get = make_pair()

local a = inc()
local b = get()
local c = inc()
local d = get()

return a, b, c, d
