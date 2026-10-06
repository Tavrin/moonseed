-- Primitive operations, exact mixed-number ordering, and fallback to __lt.
local numbers = {3, -0.0, 2.5, 9007199254740993, 9007199254740992.0, -7, 3}
table.sort(numbers)
for i = 2, #numbers do assert(numbers[i-1] <= numbers[i]) end
print(numbers[1], numbers[#numbers], math.type(numbers[#numbers]))
local strings = {'z', 'a\0z', 'a\0a', 'a', 'z', ''}
table.sort(strings)
print(strings[1] == '', strings[2] == 'a', strings[3] == 'a\0a', strings[4] == 'a\0z')
local function val(v) if type(v) == 'table' then return v.n else return v end end
local calls = 0
local mt = {__lt = function(a,b) calls = calls + 1 return val(a) < val(b) end}
local mixed = {3, setmetatable({n=1}, mt), 2, setmetatable({n=4}, mt)}
table.sort(mixed)
print(val(mixed[1]), val(mixed[4]), calls > 0)
local ok = pcall(table.sort, {3, 'x', 1})
print(ok)
return 'done'
