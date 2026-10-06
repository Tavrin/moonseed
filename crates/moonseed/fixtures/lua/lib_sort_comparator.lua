-- Reentry must observe a newly installed metatable and deleted keys.
local t = {6,5,4,3,2,1}
local reads, writes, calls = 0, 0, 0
local backing = {}
table.sort(t, function(a,b)
  calls = calls + 1
  if calls == 1 then
    for i=1,6 do backing[i] = t[i]; t[i] = nil end
    setmetatable(t, {
      __index = function(_,k) reads = reads + 1 return backing[k] end,
      __newindex = function(_,k,v) writes = writes + 1 backing[k] = v end,
      __len = function() error('length must not be read again') end
    })
  end
  return a < b
end)
print(table.concat(backing, ','), reads > 0, writes > 0, calls)
local broken = {5,4,3,2,1,6}
local n = 0
local ok, err = pcall(table.sort, broken, function(a,b)
  n = n+1
  if n == 3 then error('sort comparator sentinel', 0) end
  return a < b
end)
print(ok, err == 'sort comparator sentinel', n, table.concat(broken, ','))
local invalid = {5,4,3,2,1,6,7,8,9,10}
ok, err = pcall(table.sort, invalid, function() return true end)
print(ok, err == 'invalid order function for sorting')
return 'done'
