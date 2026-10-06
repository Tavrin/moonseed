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
local a <const>, b <const> = 10, 20
put(a, b)
local c <const>, d <const>, e <const> = 1
put(c, d, e)
local none <const>
put(none)
local x = 10
do
  local x <const> = x + 1
  put(x)
end
local k <const> = 42
local f = function() return k end
local g = function() return function() return k + 1 end end
put(f(), g()())
local t <const> = {}
t.x = 42
rawset(t, "y", 7)
t[1] = "one"
put(t.x, t.y, t[1])
local shadow <const> = 5
do local shadow = 1 shadow = 2 put(shadow) end
put(shadow)
local p <const>, q = 1, 2
q = 3
put(p, q)
local r <const>, s <close>, u <const> = 1, nil, 3
put(r, u)
do goto L local hidden <const> = 1 ::L:: end
local n = 0
for i = 1, 3 do local step <const> = i * 2 n = n + step end
put(n)
local count <const> = 3
local sum = 0
for i = 1, count do sum = sum + i end
put(sum)
return line
