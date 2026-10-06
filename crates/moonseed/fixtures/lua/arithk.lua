local maxi = 9223372036854775807
local x, f = 7, 7.5
assert(f * 2 == 15.0 and 15 / f == 2.0)
local t
t = setmetatable({}, {
    __sub = function(a, b)
        if a == t then assert(b == 2) return 12 end
        assert(a == 2 and b == t) return 21
    end,
    __div = function(a, b)
        if a == t then assert(b == 0.5) return 34 end
        assert(a == 0.5 and b == t) return 43
    end
})
return maxi + 1 == -maxi - 1, 20 - x, x % 3, f * 0.5, 0.5 / f,
    "40" + 2, 50 - "8", t - 2, 2 - t, t / 0.5, 0.5 / t
