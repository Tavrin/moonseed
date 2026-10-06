local sum = 0
for i = 1, 1400000 do
  local s = "item-" .. i .. ":end"
  sum = sum + #s
end
print("string_concat", sum)
