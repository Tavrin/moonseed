-- Only standard library natives; no custom PUC or Moonseed host extensions.
local sum = 0
local kind = ""
for i = 1, 2000000 do
  local n = math.abs(i % 257 - 128)
  n = math.max(n, math.min(i % 31, 17))
  sum = sum + math.floor(n * 0.5) + string.byte("native", i % 6 + 1)
  kind = type(n)
end
print("native_calls", sum, kind)
