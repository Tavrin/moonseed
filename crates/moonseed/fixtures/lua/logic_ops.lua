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
local log = ""
local note = function(tag, v) log = log .. tag return v end
put(nil and 42, false and 42, 0 and 42, "x" and 42)
put(nil or 42, false or "x", 0 or 42, "" or 42)
put(not nil, not false, not true, not 0, not "", not {})
local a = note("a", false) and note("b", 1)
local b = note("c", 1) or note("d", 2)
local c = note("e", nil) or note("f", 3)
local d = note("g", 1) and note("h", false)
put(a, b, c, d, log)
local t, f, n = true, false, nil
put(t or f and n, not t and f, t and f or 9, n or f and 1, not n == true, not (1 == 2), 1 < 2 and 2 < 3)
put(true and three())
put(false or three())
put(nil and three())
put((three()) or 1)
put(three() and 1)
put(three() or 1, 2)
local tbl = {}
put((tbl or 1) == tbl, (nil or tbl) == tbl, tbl and "y")
local count = 0
for i = 1, 10 do if i % 2 == 0 and i > 4 or i == 1 then count = count + 1 end end
put(count)
while not (count > 5) do count = count + 1 end
put(count)
local ok = pcall(function() return nil and error("no") end)
local ok2, e2 = pcall(function() return 1 and error("yes", 0) end)
put(ok, ok2, e2)
local mt = setmetatable({}, { __index = function() return false end, __eq = function() return true end })
put(not mt, mt and 1, mt.x or "dflt", mt == {} and "eq" or "ne")
put(((1 and nil) or (false or "z")) and not nil)
put(nil or nil or nil or false or nil or 7)
put(1 and 2 and 3 and 4 and nil and 6)
put(add(1, 2) > 2 and "big" or "small", many() and "m")
local x = 5
x = x > 3 and x * 2 or x
local y = not not x
put(x, y, not not nil)
return line
