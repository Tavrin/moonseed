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
local f = function(a, ...)
  local before1, before2 = ...
  local g = function(x, y, z)
    local p, q, r, s, t, u, v, w = 1, 2, 3, 4, 5, 6, 7, 8
    return x + y + z + p + w
  end
  local ignored = g(10, 20, 30)
  local after1, after2 = ...
  return before1, before2, after1, after2, ignored
end
put(f(0, 41, 42))
local t = {}
local c = function() return "c" end
local m = setmetatable({}, { __index = function(_, k) return k .. "!" end, __add = function(a, b) return 100 end })
local h = function(...)
  local x = m.key
  local y = m + 1
  local z = add(1, 2)
  local ok = pcall(function() error("x", 0) end)
  local ok2, e2 = xpcall(function() error("y", 0) end, function(e) return e .. "h" end)
  local ok3, e3 = pcall(function() return setmetatable({}, { __index = function() error("mm", 0) end }).k end)
  return x, y, z, ok, ok2, e2, ok3, e3, select('#', ...), ...
end
local r1, r2, r3, r4, r5, r6, r7, r8, r9, r10, r11, r12, r13 = h(t, c, "s", nil)
put(r1, r2, r3, r4, r5, r6, r7, r8, r9, r10 == t, r11 == c, r12, r13)
local mk = function(name) return setmetatable({}, { __close = function() line = line .. name end }) end
local cl = function(...)
  local x <close> = mk("[closed]")
  return ...
end
put(cl(1, nil, 3))
local cap = function(a, ...)
  local b, c = ...
  return function() return a, b, c end
end
put(cap(1, 2, 3)())
local inner = function(...)
  local g = function(...) return select('#', ...) end
  return g(...), g()
end
put(inner(1, 2))
local gen = function(...)
  local s = 0
  for i in ... do s = s + i end
  return s
end
put(gen(upto, 3))
local sum
sum = function(n, ...)
  if n == nil then return 0 end
  return n + sum(...)
end
put(sum(1, 2, 3, 4, 5))
local caught = function(...)
  local ok, e = pcall(function(...) error(select('#', ...), 0) end, ...)
  return ok, e, ...
end
put(caught("a", nil))
return line
