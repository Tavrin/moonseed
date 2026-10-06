-- Captures, anchored matches, substitutions, balanced text, and positions.
local text = "user=alice; id=1729; tags=(red,green,blue); score=42"
local sum = 0
for _ = 1, 200000 do
  local first, last, id = string.find(text, "id=(%d+)")
  local user = string.match(text, "^user=(%a+);")
  local tags = string.match(text, "%b()")
  local replaced, count = string.gsub(text, "(%a+)=(%w+)", "%2:%1")
  sum = sum + first + last + tonumber(id) + #user + #tags + #replaced + count
end
print("patterns", sum)
