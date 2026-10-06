local order = {}
local n = 0
local note = function(v) n = n + 1 order[n] = v return v end
local it = function(s, c)
  if c == nil then return s end
  return nil
end
local closed = 0
local closer = setmetatable({}, { __close = function() closed = closed + 1 end })
local got = 0
for x in note(it), note(5), note(nil), note(closer), note(9) do got = got + x end
local made = 0
local make = function()
  made = made + 1
  return it, 7, nil, closer, "ignored"
end
local got2 = 0
for x in make() do got2 = got2 + x end
local got3 = 0
for x in it, 11 do got3 = got3 + x end
local two = function() return it, 99 end
local got4 = 0
for x in two(), 13 do got4 = got4 + x end
local rest = function() return 17, nil, closer end
local got5 = 0
for x in it, rest() do got5 = got5 + x end
return n, order[1] == it, order[2], order[3], order[4] == closer, order[5], closed, got, made, got2, got3, got4, got5
