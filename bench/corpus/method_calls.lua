-- Colon calls through a shared prototype; no per-call object allocation.
local prototype = {}
function prototype:move(dx, dy)
  self.x = self.x + dx
  self.y = self.y + dy
  return self.x - self.y
end
local obj = setmetatable({ x = 0, y = 0 }, { __index = prototype })
local sum = 0
for i = 1, 4000000 do
  sum = sum + obj:move(i % 3 - 1, i % 5 - 2)
end
print("method_calls", obj.x, obj.y, sum)
