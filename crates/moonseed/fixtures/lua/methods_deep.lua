local obj = {}
function obj:f(n) if n == 0 then return self end return self:f(n - 1) end
local function loop(n) if n == 0 then return 42 end return loop(n - 1) end
local C = setmetatable({}, { __index = { step = function(self, n, acc) if n == 0 then return acc end return self:step(n - 1, acc + 1) end } })
local same = obj:f(100000) == obj
local l = loop(100000)
local r = C:step(100000, 0)
local function nontail(n) if n == 0 then return 0 end return (nontail(n - 1)) end
local ok = pcall(nontail, 1000000)
return same, l, r, ok
