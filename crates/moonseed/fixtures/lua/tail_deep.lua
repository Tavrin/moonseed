local f
f = function(n, acc) if n == 0 then return acc end return f(n - 1, acc + 1) end
local v
v = function(n, ...) if n == 0 then return select('#', ...), ... end return v(n - 1, ...) end
local even, odd
even = function(n) if n == 0 then return true end return odd(n - 1) end
odd = function(n) if n == 0 then return false end return even(n - 1) end
local mt = {}
local ca = setmetatable({}, mt)
local cb = setmetatable({}, { __call = function(self, n) if n == 0 then return "done" end return ca(n - 1) end })
mt.__call = function(self, n) return cb(n) end
local nat
nat = function(n) if n == 0 then return second(1, 2) end return nat(n - 1) end
local r
r = function(n) if n == 0 then return 0 end return (r(n - 1)) end
local ok = pcall(r, 1000000)
local x1, x2, x3, x4 = v(100000, 1, nil, 3)
local e = even(100001)
local done = ca(100000)
local two = nat(100000)
return f(100000, 0), x1, x2, x3, x4, e, done, two, ok
