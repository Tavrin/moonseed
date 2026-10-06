local t = {4, 3, 2, 1}
table.sort(t, function(a, b, missing)
  assert(missing == nil)
  local s = string.gsub('x', '.', function(c) return c, 'discard' end)
  return a < b, s
end)
print(table.concat(t, ','))
table.sort(t, function(...) local a, b = ... return a > b, nil, 'discard' end)
print(table.concat(t, ','))
local marker = {}
local ok, err = pcall(table.sort, t, function() error(marker, 0) end)
print(ok, err == marker)
local co = coroutine.create(function()
  table.sort({2, 1}, function(a, b) coroutine.yield('blocked') return a < b end)
end)
ok, err = coroutine.resume(co)
print(ok, err:match('attempt to yield across a C%-call boundary') ~= nil, coroutine.status(co))
local bad = {} for i = 1, 20 do bad[i] = i end
ok, err = pcall(table.sort, bad, function() return true end)
print(ok, err:match('invalid order function for sorting') ~= nil)
ok, err = xpcall(function() error(marker, 0) end, function(e)
  local a = {3, 1, 2} table.sort(a, function(x, y) return x < y end)
  return e == marker and table.concat(a, ',')
end)
print(ok, err)
return 'done'
