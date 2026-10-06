-- Lua __index, __add, and __call handlers. __add returns a scalar, not a table.
local meta = {
  __index = function(self, key)
    if key == "double" then return self.value * 2 end
    return 0
  end,
  __add = function(a, b) return a.value + b.value end,
  __call = function(self, n) return self.value + n end,
}
local a = setmetatable({ value = 7 }, meta)
local b = setmetatable({ value = 11 }, meta)
local sum = 0
for i = 1, 4000000 do
  sum = sum + a.double + (a + b) + b(i % 13)
end
print("metamethods", sum)
