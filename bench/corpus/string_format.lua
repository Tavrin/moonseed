local sum = 0
for i = 1, 1000000 do
  local s = string.format("item-%05d:%d;", i, i % 1000)
  sum = sum + #s
end
print("string_format", sum)
