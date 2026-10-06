local z1, z2 = pcall(function() end)
local o1, o2 = pcall(function() return 10 end)
local m1, m2, m3, m4 = pcall(function() return 10, nil, 30 end)
local obj = { x = 42 }
local ok, err = pcall(function() error(obj, 0) end)
local okn, errn = pcall(error, nil)
local okf, errf = pcall(error, false)
local oki, erri = pcall(error, 123, 0)
local callable = setmetatable({}, { __call = function(self, a, b) return a + b end })
local okc, sum = pcall(callable, 20, 22)
local oka, a1, a2 = pcall(function(a, b) return b, a end, 1, 2)

return z1, z2, o1, o2, m1, m2, m3, m4, ok, err == obj, err.x, okn, errn, okf, errf, oki, erri, okc, sum, oka, a1, a2
