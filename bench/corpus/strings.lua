-- string.format, concatenation, table.concat and gmatch over generated text.
local sum = 0
local parts = {}
for i = 1, 360000 do
  local s = string.format("item-%05d:%d;", i, i * 7 % 1000)
  parts[#parts + 1] = s
  sum = sum + #s
end
local text = table.concat(parts)
local words = 0
local total = 0
for name, value in string.gmatch(text, "(item%-%d+):(%d+);") do
  words = words + 1
  total = total + tonumber(value) + #name
end
local acc = ""
for i = 1, 3000 do
  acc = acc .. i .. ","
end
print("strings", sum, #text, words, total, #acc)
