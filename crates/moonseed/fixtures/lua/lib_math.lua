-- The math library (ADR 0032): results with their subtypes, and
-- argument errors by status only.
local function show(...) local t = table.pack(...) local out = {} for i = 1, t.n do local v = t[i] out[i] = (v ~= v) and "NaN" or tostring(v) .. ":" .. (math.type(v) or type(v)) end print(table.concat(out, " ")) end
show(math.tointeger("8"), math.tointeger(" 0x10 "), math.tointeger("3.0"), math.tointeger(3.5), math.tointeger(2^63), math.tointeger({}), (pcall(math.tointeger)))
show(math.type("1"), math.type(1), math.type(1.0), (pcall(math.type)))
show(math.abs("-3"), math.abs(-3), math.abs(math.mininteger), math.abs(-0.0), math.floor("3"), math.floor(" 0x10 "), math.floor(-3.5), math.floor(2^63), math.ceil(1e308), math.floor(0/0))
show(math.fmod(7, 3), math.fmod(-7, 3), math.fmod(7, -3), math.fmod(math.mininteger, -1), (pcall(math.fmod, 1, 0)), math.fmod(1, 0.0), math.fmod(-7.5, 2), math.fmod(1/0, 1), math.fmod(1, 1/0), math.fmod(-0.0, 1))
show(math.modf(3)) show(math.modf(-3.5)) show(math.modf(1/0)) show(math.modf(-1/0)) show(math.modf(-0.0)) show(math.modf(0/0)) show(math.modf(2^63)) show(math.modf("2.5"))
show(math.ult(1, -1), math.ult(-1, 1), math.min(3, 1, 2), math.max("b", "a"), math.min(1, 1.0), math.max(1.0, 1), (pcall(math.min)), (pcall(math.min, 1, "x")))
show(math.log(8, 2), math.log(100, 10), math.log(27, 3), math.log(0), math.log(-1), math.log(2, 1), math.log(1, 1))
show(math.pi, math.huge, -math.huge, math.maxinteger, math.mininteger)
show(math.sqrt(2), math.sqrt(4), math.exp(1), math.sin(1), math.cos(1), math.tan(1), math.asin(0.5), math.acos(0.5), math.atan(1, 2), math.atan(1), math.atan(-1, -1), math.deg(math.pi), math.rad(180), math.exp(710), math.log(2^-1074))
show(math.max(1, 2, 3.5, -1), math.min(-0.0, 0.0), math.max(math.mininteger, math.maxinteger))
show(math.floor(-0.0), math.ceil(-0.5), math.ceil(0.5), math.floor(1e300), math.ceil(-2^63), math.floor(2^62))
return "done"
