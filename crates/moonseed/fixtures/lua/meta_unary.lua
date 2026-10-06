local seen = {}
local t = setmetatable({}, {
    __len = function(a, b, c)
        seen[1], seen[2] = a, b
        return c == nil
    end,
    __unm = function(a, b, c)
        seen[3], seen[4] = a, b
        return "neg"
    end,
    __bnot = function(a, b, c)
        seen[5], seen[6] = a, b
        return c == nil
    end
})
local third_is_nil = #t
local negated, flipped = -t, ~t

return third_is_nil, seen[1] == t, seen[2] == t, negated, seen[3] == t, seen[4] == t, flipped, seen[5] == t, seen[6] == t
