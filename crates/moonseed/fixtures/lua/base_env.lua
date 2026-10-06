-- `_G`, `_VERSION`, `type`, and `assert` (ADR 0031).
local G = _G
print(_G == _ENV, _VERSION, rawequal(_G, G))
_G = "other"
print(_G, G._G, rawget(G, "_G"))
print(x == nil, G.x == nil)
G._G = G
print(type(nil), type(false), type(0), type(0.5), type(""), type({}))
print(type(print), type(function() end), type(type), type(G))
print((pcall(type)))
print(type(nil) == "nil", type(setmetatable({}, { __name = "Named" })))
print(select("#", assert(10, 20, nil, 30)), assert(10, 20, nil, 30))
print(select("#", assert(true)), assert("v", "m"))
print(pcall(assert, false))
print(pcall(assert, nil, 42))
print(select("#", pcall(assert, false, nil)), pcall(assert, false, nil))
print(pcall(assert, false, "message", "extra"))
local err = {}
local ok, e = pcall(assert, false, err)
print(ok, e == err)
print((pcall(assert)))
return "done"
