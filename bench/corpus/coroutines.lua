-- Resume/yield ping-pong with one long-lived coroutine and integer messages.
local co = coroutine.create(function(value)
  local sum = 0
  for i = 1, 3000000 do
    sum = sum + value
    value = coroutine.yield(i + value)
  end
  return sum
end)
local sum = 0
for i = 1, 3000000 do
  local ok, value = coroutine.resume(co, i % 17)
  assert(ok, value)
  sum = sum + value
end
local ok, total = coroutine.resume(co, 0)
assert(ok, total)
assert(coroutine.status(co) == "dead")
print("coroutines", sum, total)
