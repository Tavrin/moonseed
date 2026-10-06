-- The table library (ADR 0033): results, boundary cases, metamethod
-- order, and argument errors by status only.
local function status(...) return (pcall(...)) end
local function dump(t, n) local out = {} for i = 1, n or #t do out[#out + 1] = tostring(t[i]) end return table.concat(out, ",") end
local t = {1, 2, 3}
table.insert(t, 4) table.insert(t, 1, 0) table.insert(t, 3, "x") print(dump(t))
print(status(table.insert, t, 9, 1), status(table.insert, t, 0, 1), status(table.insert, t, 1, 2, 3), status(table.insert, t))
table.insert(t, #t + 1, "end") print(dump(t))
print(table.remove(t), table.remove(t, 1), dump(t))
print(table.remove({}), table.remove({}, 0), table.remove({}, 1), status(table.remove, {}, 2))
local r = {1, 2} print(table.remove(r, 3), dump(r), status(table.remove, r, 4))
local z = {[0] = "zero"} print(table.remove(z, 0), z[0])
print(table.concat({}), table.concat({1, 2, 3}), table.concat({1, "a", 2.5}, "-"), table.concat({1, 2, 3}, ", ", 2, 3), table.concat({1, 2}, "x", 3), table.concat({"a"}, 1, 1, 1))
print(status(table.concat, {1, {}, 3}), status(table.concat, {1, 2}, {}))
print(select('#', table.unpack({})), table.unpack({1, nil, 3}), table.unpack({1, 2, 3}, 2), table.unpack({1, 2, 3}, 2, 5))
print(select('#', table.unpack({}, 1, 0)), select('#', table.unpack({}, 5, 3)), status(table.unpack, {}, 1, 1e8), status(table.unpack, {}, math.mininteger, math.maxinteger))
print(table.unpack({1, 2, 3}, -1, 1))
local p = table.pack() print(p.n, #p) p = table.pack(1, nil, 3) print(p.n, p[1], p[2], p[3])
local m = {1, 2, 3, 4, 5} table.move(m, 1, 3, 2) print(dump(m))
m = {1, 2, 3, 4, 5} table.move(m, 2, 4, 1) print(dump(m))
m = {1, 2, 3} local d = table.move(m, 1, 3, 3, {}) print(dump(d, 5), d == m)
print(dump(table.move({1, 2}, 1, 0, 1)), status(table.move, {}, 1, math.maxinteger, 2), status(table.move, {}, math.mininteger, 1, 1), status(table.move, {}, 1, 2, math.maxinteger))
print(table.move({1,2}, 1, 2, 1) ~= nil, status(table.move, 1, 1, 1, 1), status(table.move, {}, 1, 1, 1, 5))
local proxy = setmetatable({}, {__index = function(t, k) return k * 10 end, __len = function() return 3 end})
print(table.concat(proxy, ","), table.unpack(proxy))
local log = {}
local w = setmetatable({}, {__index = function(t, k) log[#log + 1] = "g" .. k return rawget(t, "_" .. k) end, __newindex = function(t, k, v) log[#log + 1] = "s" .. k rawset(t, "_" .. k, v) end, __len = function() return 3 end})
table.insert(w, 1, "v") print(table.concat(log, " "))
log = {} table.remove(w, 1) print(table.concat(log, " "))
print(status(table.insert, setmetatable({}, {__len = function() return 1.5 end}), 1))
print(status(table.insert, setmetatable({}, {__len = function() return "2" end}), 1))
local tt = setmetatable({}, {__len = function() return "x" end}) print(status(table.insert, tt, 1))
print(status(table.insert, 1, 1), status(table.concat, "abc"), status(table.sort, nil))
return "done"
