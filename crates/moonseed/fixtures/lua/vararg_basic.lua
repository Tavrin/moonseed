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
local f = function(a, b, ...) return a, b, ... end
put(f())
put(f(1))
put(f(1, 2))
put(f(1, 2, 3, 4))
put(f(1, 2, nil, nil))
local count = function(...) return select('#', ...) end
put(count(), count(nil), count(nil, nil))
local first = function(...) local x = ... return x end
put(first(), first(7, 8))
local three = function(...) local a, b, c = ... return a, b, c end
put(three(1))
put(three(1, 2, 3, 4))
local paren = function(...) return (...) end
put(paren(5, 6))
put(paren())
local plus = function(...) return 1 + ... end
put(plus(2, 3))
local tail = function(x, ...) return x, ... end
put(tail(1, nil, 3))
local mid = function(...) return ..., "x" end
put(mid(1, 2))
local pass = function(...) return count(...) end
put(pass(nil, nil, nil))
local passmid = function(...) return count(..., 9) end
put(passmid(1, 2, 3))
local ctor = function(...) local t = { ... } return #t, t[1], t[3] end
put(ctor(1, 2, 3))
local ctor2 = function(...) local t = { ..., 5 } return t[1], t[2], t[3] end
put(ctor2(1, 2, 3))
local holes = function(...) local t = { ... } return t[1], t[2], t[3] end
put(holes(nil, 2, nil))
local sel = function(...) return select(-1, ...) end
put(sel(1, 2, 3))
put(select(2, "a", "b", "c"))
put(select(5, "a"))
put(select("2", "a", "b"))
put(select(2.0, "a", "b"))
put(select('#'))
put(select('#x', 1, 2))
put(pcall(select, 0, 1) == false, pcall(select, -3, 1) == false, pcall(select, 1.5, 1) == false, pcall(select, "x", 1) == false, pcall(select) == false)
local assign = function(...) local a, b = 0, 0 a, b = ... return a, b end
put(assign(4))
local cond = function(...) if ... then return "yes" end return "no" end
put(cond(false, true), cond(0))
return line
