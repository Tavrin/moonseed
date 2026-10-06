local first
local second
local iter = function(_, control)
  if control == nil then return 1, "first" end
  if control == 1 then return 2, "second" end
  return nil
end
for k, v in iter do
  if k == 1 then
    first = function() return k, v end
  else
    second = function() return k, v end
  end
end
local fs = {}
local m = 0
for k in iter do
  m = m + 1
  local j = k * 10
  fs[m] = function() k = k + 1 return k + j end
end
local a, b = first()
local c, d = second()
return a, b, c, d, fs[1](), fs[1](), fs[2]()
