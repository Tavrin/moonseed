local all = function(...) return ... end
local g = function() error("boom", 0) end
local f = function() return g() end
local ok, e = pcall(f)
local okx, ex = xpcall(f, function(m) return "H" .. m end)
local h = function() error("in", 0) end
local g2 = function() local ok, e = pcall(h) error(e .. "!", 0) end
local f2 = function() return g2() end
local ok2, e2 = pcall(f2)
local f3 = function() return pcall(g) end
local ok3a, ok3b, e3 = pcall(f3)
local f4 = function(...) return error(...) end
local ok4, e4 = pcall(f4, "four", 0)
local idx = setmetatable({}, { __index = function(t, k) return add(k, 1) end })
local okh, eh = xpcall(f, function(m) return g2() end)
return all(ok, e, okx, ex, ok2, e2, ok3a, ok3b, e3, ok4, e4, idx[41], okh, eh)
