local calls = 0
local mt_a = { __eq = function(a, b) calls = calls + 1 return 1 end }
local mt_b = { __eq = function(a, b) calls = calls + 1 return nil end }
local a1, a2 = setmetatable({}, mt_a), setmetatable({}, mt_a)
local b1 = setmetatable({}, mt_b)
local plain = {}

local same = a1 == a1
local after_same = calls
local ab = a1 == b1
local ba = b1 == a1
local ap = a1 == plain
local pa = plain == a1
local ne = a1 ~= a2
local raw = rawequal(a1, a2)
local raw_same = rawequal(a1, a1)
local mixed = a1 == 1

local order = { __lt = function(a, b) return 1 end, __le = function(a, b) return nil end }
local x, y = setmetatable({}, order), setmetatable({}, order)
local left = setmetatable({}, { __lt = function(a, b) return a == 1 end })

return same, after_same, ab, ba, ap, pa, ne, raw, raw_same, mixed, calls,
    x < y, x > y, x <= y, x >= y, 1 < left, left < 2, 2 > left, "a" < "b", 1 < 1.5, 2 <= 2.0
