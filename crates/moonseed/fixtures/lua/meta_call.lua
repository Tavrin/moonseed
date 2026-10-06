local seen
local t = setmetatable({}, {
    __call = function(self, a, b)
        seen = self
        return a + b, self
    end
})
local x, y = t(20, 22)

local noargs = setmetatable({}, { __call = function(self, a) return a == nil end })
local many = setmetatable({}, { __call = function(self) return 1, 2, 3 end })
local packed = { many() }
local first = many()

local f = function(p1, p2, p3, p4, p5, p6)
    return p1, p2, p3, p4, p5, p6
end
local c = setmetatable({}, { __call = f })
local d = setmetatable({}, { __call = c })
local e = setmetatable({}, { __call = d })
local q1, q2, q3, q4, q5, q6 = e(1, 2)

local callable = setmetatable({}, { __call = function(self, a, b) return b + 100 end })
local plus = setmetatable({}, { __add = callable })
local via_add = plus + 1

local target = setmetatable({ k = "field" }, { __call = function() return "called" end })
local indexed = setmetatable({}, { __index = target })

local native = setmetatable({}, { __call = second })

return x, y == t, seen == t, noargs(), #packed, packed[3], first, q1 == c, q2 == d, q3 == e, q4, q5, q6, via_add, indexed.k, indexed.other, native(9)
