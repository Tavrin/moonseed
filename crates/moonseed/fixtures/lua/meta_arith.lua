local lhs = setmetatable({}, { __add = function(a, b) return "lhs" end })
local rhs = setmetatable({}, {
    __add = function(a, b) return "rhs" end,
    __sub = function(a, b) return a end
})
local picks = { lhs + rhs, rhs + lhs, 1 + rhs, lhs + 1, 1 - rhs }

local ev = setmetatable({}, {
    __add = function() return "add" end,
    __sub = function() return "sub" end,
    __mul = function() end,
    __div = function() return "div", "ignored" end,
    __idiv = function() return "idiv" end,
    __mod = function() return "mod" end,
    __pow = function() return "pow" end,
    __band = function() return "band" end,
    __bor = function() return "bor" end,
    __bxor = function() return "bxor" end,
    __shl = function() return "shl" end,
    __shr = function() return "shr" end,
    __concat = function() return "concat" end
})
local native = setmetatable({}, { __add = second, __band = second })
local callable = setmetatable({}, { __call = function(self, a, b) return 7 end })
local called = setmetatable({}, { __mul = callable })

return picks[1], picks[2], picks[3], picks[4], picks[5], ev + 1, ev - 1, ev * 1, ev / 1, ev // 1, ev % 1, ev ^ 1,
    ev & 1, ev | 1, ev ~ 1, ev << 1, ev >> 1, 1.5 & ev, "x" .. ev, native + 5, native & 1.5, called * 2
