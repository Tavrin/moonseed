-- Shared mutable upvalues, captured independently by 32 pairs of closures.
local function counter(initial)
  local value = initial
  local function add(delta)
    value = value + delta
    return value
  end
  local function read() return value end
  return add, read
end
local adders, readers = {}, {}
for i = 1, 32 do adders[i], readers[i] = counter(i) end
local sum = 0
for i = 1, 10000000 do
  local slot = i % 32 + 1
  sum = sum + adders[slot](i % 7 - 3)
end
local total = 0
for i = 1, 32 do total = total + readers[i]() end
print("closures", sum, total)
