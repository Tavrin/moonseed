-- `load` of text chunks (ADR 0031).
print(pcall(load, function() error("boom", 0) end))
local et = {}
print(select(3, pcall(load, function() error(et) end)) == et)
local n = 0
print(select("#", load(function() n = n + 1 if n < 3 then return 1 end end)), n)
print(pcall(load, function() return {} end))
local parts = { "return ", "1 ", "+ 2", "", "ignored" }
local i = 0
print(load(function() i = i + 1 return parts[i] end)(), i)
print(load("return 1", "=name", "b"))
print(load("\27Lua", "x", "t"))
print((pcall(load, setmetatable({}, { __call = function() return nil end }))))
print(pcall(load, "return 1", nil, 5))
print(load("return 1", nil, "q"))
print(load("return 1", nil, "t")(), load("return 2", nil, "bt")(), load("return 3", nil, "tb")())
local env = { y = 5 }
print(load("return y", "c", "t", env)())
local f = load("return y", "c", "t", nil)
print(type(f), (pcall(f)))
print(load("local a <const> = 1; return ...")(7, 8))
print(type(load(function() return nil end)), type(load("")), select("#", load("return")()))
print(load("return _ENV")() == _G)
x_global = 11
print(load("return x_global")())
print(load("x_global = 12")(), x_global)
print(load("local a, b = ... return b, a")(1, 2))
local sandbox = {}
load("z = 1", "=s", "t", sandbox)()
print(sandbox.z, z)
local pieces = { "local s = 0 ", "for i = 1, 10 do ", "s = s + i ", "end ", "return s" }
local j = 0
print(load(function() j = j + 1 return pieces[j] end, "=pieces")())
local k = 0
print(load(function() k = k + 1 if k == 1 then return "return " elseif k == 2 then return 40 + 2 end end)())
local loaded = load("return function(a) return a * 2 end")()
print(loaded(21))
print(load("return 1", 5)())
-- The mode is checked on the reader's first piece, and ends at a zero byte.
local calls = 0
print(load(function() calls = calls + 1 return "\27Lua" end, "x", "t"))
print(calls)
print(load("return 'a'", "=x", "b\0t"))
-- `load` keeps the message handler of the `xpcall` it runs in; `pcall`
-- has none.
local function failing() error("rd", 0) end
local function handler(m) return "H:" .. m end
print(xpcall(function() return load(failing) end, handler))
print(pcall(function() return load(failing) end))
print(xpcall(function() return pcall(function() return load(failing) end) end, handler))
local ok, f, m = xpcall(function() return load(function() return {} end) end, function(m) return { m } end)
print(ok, f, type(m))
return "done"
