-- `tostring` and `print` (ADR 0031).
print(tostring(nil), tostring(true), tostring(false), tostring(1), tostring(-7))
print(tostring(0.5), tostring(-0.0), tostring(1e100), tostring(2^63), tostring(1/0), tostring(-1/0))
print(tostring(3.0), tostring(1e15), tostring(123456789012345.0), tostring(-3.5), tostring("str"))
local t = {}
print(tostring(t) == tostring(t), tostring(t) ~= tostring({}), tostring(print) == tostring(print))
local T = setmetatable({}, { __tostring = function() return "T" end })
print(tostring(T), T)
print(tostring(setmetatable({}, { __tostring = function() return 42 end })))
print(pcall(tostring, setmetatable({}, { __tostring = function() return nil end })))
print(pcall(tostring, setmetatable({}, { __tostring = function() return {} end })))
print(tostring(setmetatable({}, { __tostring = function() return "a", "b" end })))
print(tostring(setmetatable({}, { __tostring = function() return "N" end, __name = "X" })))
print(select("#", tostring(1, 2, 3)))
print((pcall(tostring)))
local calls = 0
local counted = setmetatable({}, { __tostring = function(self) calls = calls + 1 return "c" .. calls end })
print(counted, counted, tostring(counted), calls)
local callable = setmetatable({}, { __tostring = setmetatable({}, { __call = function(self, v) return "via call" end }) })
print(callable)
-- `print` converts as `tostring` does, but never calls the global.
local saved = tostring
tostring = function() return "X" end
print(1, nil, true, T, 2.5)
tostring = saved
-- Written before a later argument's error, without the newline.
print(pcall(print, 1, 2, setmetatable({}, { __tostring = function() return {} end }), 4))
print(pcall(print, "a", setmetatable({}, { __tostring = function() error("E", 0) end })))
-- A `__tostring` that prints writes after the arguments before it.
print(1, setmetatable({}, { __tostring = function() print("inner") return "x" end }), 3)
print()
print(select("#", print("a", "b")))
print("", "", "")
return "done"
