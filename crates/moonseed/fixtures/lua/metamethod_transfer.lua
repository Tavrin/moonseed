-- Yield in each direct handler, then change the handlers while suspended.
local events = 0
local function named(event)
  local info = debug.getinfo(2, 'n')
  assert(info.namewhat == 'metamethod' and info.name == event)
  events = events + 1
end
local meta = {
  __index = function(self, key)
    named('index')
    assert(key == 'double')
    coroutine.yield('index', self.value)
    return self.value * 2
  end,
  __add = function(a, b)
    named('add')
    coroutine.yield('add', a.value, b.value)
    return a.value + b.value
  end,
  __call = function(self, n)
    coroutine.yield('call', n)
    events = events + 1
    return self.value + n, nil, 'extra'
  end,
}
local a = setmetatable({value = 7}, meta)
local b = setmetatable({value = 11}, meta)
local co = coroutine.create(function()
  local sum = a.double + (a + b) + b(3)
  -- The new methods must be observed immediately, even after cached setup.
  sum = sum + a.double + (a + b) + b(4)
  return sum, events
end)
local ok, event, x, y = coroutine.resume(co)
assert(ok and event == 'index' and x == 7 and y == nil)
meta.__index = function() return 21 end
ok, event, x, y = coroutine.resume(co, nil, true, 5)
assert(ok and event == 'add' and x == 7 and y == 11)
meta.__add = function() return 22 end
ok, event, x, y = coroutine.resume(co)
assert(ok and event == 'call' and x == 3 and y == nil)
meta.__call = function() return 23 end
local sum, count
ok, sum, count = coroutine.resume(co)
assert(ok and sum == 112 and count == 3)
assert(coroutine.status(co) == 'dead')
-- Open/padded/discarded windows, wrapped transfers, and a chained callable.
local chain = setmetatable({}, {__call = b})
assert(chain() == 23)
local wrapped = coroutine.wrap(function(...)
  local n = select('#', ...)
  local x, y, z = coroutine.yield(n, nil, false)
  assert(x == 9 and y == nil and z == true)
  return x, y, z
end)
local n, hole, flag, pad = wrapped(1, nil, 3)
assert(n == 3 and hole == nil and flag == false and pad == nil)
wrapped(9, nil, true)
return sum, count
