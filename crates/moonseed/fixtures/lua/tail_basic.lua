local show = function(v)
  if v == nil then return "nil" end
  if v == true then return "true" end
  if v == false then return "false" end
  return v
end
local line = ""
local put = function(...)
  local k = select('#', ...)
  local s = "(" .. k .. ")"
  for i = 1, k do s = s .. " " .. show((select(i, ...))) end
  line = line .. s .. ";"
end
local three = function() return 10, nil, 30 end
local g = function() return three() end
local n = function() return many() end
put(g())
local a, b = g()
put(a, b)
local c = g()
put(c)
g()
put((g()))
local t = { g() }
put(t[1], t[2], t[3])
local t2 = { g(), 1 }
put(t2[1], t2[2], t2[3])
put(n())
local x, y = n()
put(y, x)
local iter = function() return upto, 3 end
local init = function() return iter() end
local s = 0
for i in init() do s = s + i end
put(s)
local fixed = function(p, q) return p, q end
local var = function(...) return select('#', ...), ... end
put((function(a, b, c) return fixed(a, b, c) end)(1, 2, 3))
put((function(a, b) return var(a, b) end)(1, nil))
put((function(...) return fixed(...) end)(1, nil, 3))
put((function(a, ...) return var(...) end)(0, 10, nil, 30))
put((function(a, ...) return var(1, ...) end)(0, 10, nil, 30))
put((function(...) return var() end)(1, 2))
put((function() return (three()) end)())
put((function() return 1, three() end)())
put((function() return three(), 1 end)())
local saved
local rd = function() return saved() end
local mk = function() local v = 42 saved = function() return v end return rd() end
put(mk())
local callable = setmetatable({}, { __call = function(self, p, q) return q, p, self == callable end })
put((function(...) return callable(...) end)(1, 2))
put((function() return second(1, nil) end)())
put((function(...) return add(...) end)(2, 3))
local order = function(tag) line = line .. tag end
local closer = function(tag) return setmetatable({}, { __close = function() order(tag) end }) end
local logged = function() order("g") return "r" end
local withclose = function() local x <close> = closer("x") return logged() end
put(withclose())
local afterclose = function() do local x <close> = closer("y") end return logged() end
put(afterclose())
local closed = 0
local cmt = { __close = function() closed = closed + 1 end }
local rec
rec = function(k) local cv <close> = setmetatable({}, cmt) if k == 0 then return 0 end return rec(k - 1) end
put(rec(50), closed)
return line
