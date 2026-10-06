-- table.sort (ADR 0033): default and custom orders, invalid order
-- functions, comparison counts, and a comparator that changes the list.
local function status(...) return (pcall(...)) end
local function dump(t, n) local out = {} for i = 1, n or #t do out[#out + 1] = tostring(t[i]) end return table.concat(out, ",") end
local s = {5, 2, 8, 1, 9, 3} table.sort(s) print(dump(s))
s = {5, 2, 8, 1, 9, 3} table.sort(s, function(a, b) return a > b end) print(dump(s))
s = {"b", "a", "c"} table.sort(s) print(dump(s))
print(status(table.sort, {3, 1, "x"}), status(table.sort, {1, 2}, 5), status(table.sort, {1}, 5), status(table.sort, {1, 2, 3}, setmetatable({}, {__call = function() return true end})))
print(status(table.sort, {5, 4, 3, 2, 1, 6, 7, 8, 9, 10}, function(a, b) return true end))
local big = {} for i = 1, 40 do big[i] = (i * 7919) % 503 end table.sort(big) local ok = true for i = 2, 40 do if big[i - 1] > big[i] then ok = false end end print(ok, big[1], big[40])
local calls = 0 local eq = {} for i = 1, 20 do eq[i] = i % 3 end table.sort(eq, function(a, b) calls = calls + 1 return a < b end) print(calls, eq[1], eq[20])
local mut = {5, 4, 3, 2, 1, 6, 7, 8} print(status(table.sort, mut, function(a, b) mut[1] = 99 return (a or 0) < (b or 0) end))
return "done"
