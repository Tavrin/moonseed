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
local n = 0
::again::
do
  n = n + 1
  if n < 3 then goto again end
end
put(n)
local saved, count = nil, 0
::again2::
do
  local x = count
  saved = function() return x end
  count = count + 1
  if count < 2 then goto again2 end
end
put(saved(), count)
local fs, k = {}, 0
::top::
do
  local x = k
  fs[#fs + 1] = function() return x end
  k = k + 1
  if k < 3 then goto top end
end
put(fs[1](), fs[2](), fs[3]())
local s = 0
for i = 1, 5 do
  if i % 2 == 0 then goto continue end
  s = s + i
  ::continue::
end
local w, j = 0, 0
while j < 5 do
  j = j + 1
  if j == 3 then goto next end
  w = w + j
  ::next::
end
local r = 0
repeat
  r = r + 1
  if r < 3 then goto skip end
  ::skip::
until r >= 5
put(s, w, r)
do goto L; local x = 10; ::L:: end
do goto M; local y = 10; ::M:: ; ; ::N:: end
local t = {}
for i = 1, 3 do
  local x = i
  t[i] = function() return x end
  if i == 2 then goto done end
end
::done::
put(t[1](), t[2](), t[3])
local a = 1
::back::
local b = a * 10
if a < 3 then a = a + 1 goto back end
put(a, b)
local order = ""
goto first
::second::
order = order .. "2"
goto third
::first::
order = order .. "1"
goto second
::third::
put(order)
local depth = 0
do
  do
    do
      depth = 3
      goto out
    end
  end
end
::out::
put(depth)
local found
for x in upto, 5 do
  for y in upto, 5 do
    if x * y == 6 then found = x .. "*" .. y goto found_it end
  end
end
::found_it::
put(found)
local fs = {}
while true do
  local x = 0
  ::retry1::
  if x == 1 then break end
  fs[#fs + 1] = function() return x end
  x = x + 1
  goto retry1
end
local y = 100
repeat
  local x = 10
  ::retry2::
  if x == 11 then break end
  fs[#fs + 1] = function() return x end
  x = x + 1
  goto retry2
until true
local z = 200
for i = 1, 1 do
  local x = 20
  ::retry3::
  if x == 21 then break end
  fs[#fs + 1] = function() return x end
  x = x + 1
  goto retry3
end
local p, q, w, v = 100, 200, 300, 400
put(fs[1](), fs[2](), fs[3]())
return line
