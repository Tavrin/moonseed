-- math.random and math.randomseed (ADR 0032): sequences from explicit
-- seeds, every argument form, and argument errors by status only.
local function show(...) local t = table.pack(...) local out = {} for i = 1, t.n do local v = t[i] out[i] = (v ~= v) and "NaN" or tostring(v) .. ":" .. (math.type(v) or type(v)) end print(table.concat(out, " ")) end
show(math.randomseed(123, 456))
for i = 1, 3 do show(math.random(), math.random(0), math.random(10), math.random(-5, 5), math.random(math.mininteger, math.maxinteger)) end
show(math.randomseed(7))
show(math.random(1, 3), (pcall(math.random, 2, 1)), (pcall(math.random, 1, 2, 3)), math.random(3))
show((pcall(math.randomseed, 1.5)), (pcall(math.randomseed, "2")))
math.randomseed(42) local s = 0 for i = 1, 12 do s = s + math.random(1, 1000003) end show(s)
math.randomseed(-1, -1) local c = 0 for i = 1, 12 do c = c ~ math.random(0) end show(c)
math.randomseed(9, 9) local f = 0 for i = 1, 12 do f = f + math.random() end show(f)
return "done"
