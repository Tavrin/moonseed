local s, n = string.gsub('a-b', '%a', function(c, missing)
  assert(missing == nil)
  return '[' .. c .. ']', 'discard'
end)
print(s, n)
s, n = string.gsub('ab', '.', function(...) local c = ... return c == 'a' and false or nil end)
print(s, n)
local marker, calls = {}, 0
local ok, err = pcall(string.gsub, 'ab', '.', function()
  calls = calls + 1 if calls == 2 then error(marker, 0) end return 'X'
end)
print(ok, err == marker, calls)
local co = coroutine.create(function()
  return string.gsub('x', '.', function() coroutine.yield('blocked') return 'X' end)
end)
ok, err = coroutine.resume(co)
print(ok, err:match('attempt to yield across a C%-call boundary') ~= nil, coroutine.status(co))
local pieces, i = {'return ', '17, nil, 19'}, 0
local f = assert(load(function() i = i + 1 return pieces[i] end))
print(f())
local mt = {__pairs = function(t) return next, t, nil, 'discard' end}
local count = 0 for k, v in pairs(setmetatable({x = 1}, mt)) do count = count + v end
print(count)
return 'done'
