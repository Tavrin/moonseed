-- Immediate results, register windows, numeric boundaries and canonical fallback.
local a, b, c = math.abs(-7)
print(a, b, c, select('#', math.floor(3.5)))
math.abs(-8) -- discard a result
print(math.min(9007199254740993, 9007199254740992.0), math.max(9007199254740993, 9007199254740992.0))
print(1 / math.min(-0.0, 0.0), math.max(0/0, 1) ~= math.max(0/0, 1))
print(string.byte('abc', -1), string.byte('abc', 0), string.byte('abc', 9), string.byte('abc', 2, 2))
print(string.byte('abc', 1, 3), string.byte(123, 2), string.byte('abc', '2'))
local args = {} for i = 1, 33 do args[i] = i end
print(math.max(table.unpack(args, 1, 32)), math.max(table.unpack(args, 1, 33)))
local mt = {__lt = function(x, y) return x[1] < y[1] end}
print(math.max(setmetatable({2}, mt), setmetatable({3}, mt))[1])
for _, f in ipairs({function() local v = math.abs({}); return v end,
                   function() local v = math.floor(false); return v end,
                   function() local v = type(); return v end,
                   function() local v = string.byte({}, 1); return v end}) do
  local ok, err = pcall(f)
  print(ok, (err:match('bad argument.*'):gsub("'math%.", "'"):gsub("'string%.", "'")))
end
local function tail() return math.abs(-9) end
print(tail(), type(nil), math.abs('-4'), math.ceil(2.25))
return 'done'
