-- The string library's direct functions, the string metatable, and string
-- arithmetic (ADR 0034). Output must match Lua 5.4.9.
local function show(...)
  local out = {}
  for i = 1, select('#', ...) do
    local v = select(i, ...)
    -- `%q` keeps bytes past 127 as they are; write them as escapes.
    out[#out + 1] = type(v) == "string"
        and (string.gsub(string.format("%q", v), "[\128-\255]", function(c) return "\\" .. c:byte() end))
      or tostring(v)
  end
  print(table.concat(out, " "))
end
-- An error's message without the position Lua puts before it.
local function fails(ok, message)
  return ok, (string.gsub(message, "^[^:]*:%d+: ", ""))
end
local mt = getmetatable("")
show(mt.__index == string, getmetatable("a") == getmetatable("b"), getmetatable(1), getmetatable(print))
show(("hello"):sub(2), string.sub("hello", 2, -2), ("hello"):sub(-3), ("x"):sub(0), ("abc"):sub(5), ("abc"):sub(2, 1))
show(("abc"):sub(math.mininteger, math.maxinteger), ("abc"):sub(-100, 100), ("abc"):sub(3, -1), ("abc"):sub(-1, 1))
show(("a\0b\0"):len(), #"a\0b", string.len(123), string.len(-1.5), ("\0"):rep(3, "\1"))
show(("MiXeD \200"):upper(), ("MiXeD \200"):lower(), ("abc\0d"):reverse(), (""):reverse())
show(("ab"):rep(3), ("ab"):rep(3, ", "), ("x"):rep(0), ("x"):rep(-5), ("x"):rep(1, "sep"), (""):rep(100))
show(string.byte("ABC"), string.byte("ABC", 2), string.byte("ABC", -1), string.byte("ABC", 1, -1))
show(string.byte("ABC", 0), string.byte("ABC", 10), string.byte("", 1), string.byte("\255\0", 1, 2))
show(string.char(), string.char(72, 105, 0, 255), string.char("65"))
show(pcall(string.char, 256))
show(pcall(string.char, -1))
show(pcall(string.rep))
show(pcall(string.sub, "x", 1.5))
show(pcall(string.upper, {}))
show(pcall(string.byte, "x", "y"))
show(string.upper(12), string.rep(1, 3), string.reverse(1.5))
show("10" + 1, "0x10" * 2, "1e1" // 3, -"2", "7" % "4", "2" ^ "3", "3" / "2", " 5 " - 1)
show(fails(pcall(function() return "abc" + 1 end)))
show(fails(pcall(function() return {} + "1" end)))
show(fails(pcall(function() return "1" - {} end)))
show((pcall(function() return "10" // "0" end)))
show((pcall(function() return "1.5" & 1 end)))
show("1" + setmetatable({}, { __add = function(a, b) return "right" end }))
show(setmetatable({}, { __sub = function(a, b) return "left" end }) - "1")
local add = mt.__add
mt.__add = function(a, b) return "patched" end
show("a" + 1, 1 + "b", "3" - 1)
mt.__add = nil
show((pcall(function() return "40" + 2 end)))
mt.__add = add
show("40" + 2)
local index = mt.__index
mt.__index = function(s, k) return k .. "!" end
show(("x").foo, ("y")[1])
mt.__index = { up = string.upper }
show(("q"):up(), (pcall(function() return ("q"):sub(1) end)))
mt.__index = index
show(("restored"):upper())
show((pcall(setmetatable, "x", {})))
return "done"
