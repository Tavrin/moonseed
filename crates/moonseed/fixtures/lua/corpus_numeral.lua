-- Long numerals through `tonumber` and `load` (Phase 3.24), compared line by
-- line with Lua 5.4.9. Floats print as `%a`, so every bit is compared.
local function show(label, v)
  if math.type(v) == "float" then
    print(label, "float", v ~= v and "nan" or string.format("%a", v))
  else
    print(label, math.type(v), v)
  end
end
local lengths = { 1, 10, 100, 1000, 10000, 100000, (1 << 20) - 64 }
for _, n in ipairs(lengths) do
  local shapes = {
    string.rep("9", n),
    string.rep("1", n) .. ".5",
    "0." .. string.rep("0", n) .. "1",
    "1" .. string.rep("0", n) .. "e-" .. n,
    "." .. string.rep("3", n),
    "0x" .. string.rep("0", n) .. "a",
    "0x" .. string.rep("f", n),
    "0x" .. string.rep("f", n) .. ".0",
    "0x0." .. string.rep("0", n) .. "1",
    "0x1" .. string.rep("0", n) .. "p-" .. (4 * n),
    "0x." .. string.rep("0", n) .. "74p" .. (4 * n + 4),
    "0x" .. string.rep("7", n) .. "p-" .. n,
  }
  for index, text in ipairs(shapes) do
    if #text <= (1 << 20) then
      show(n .. ":" .. index, tonumber(text))
      if #text < 200000 then
        local f = load("return " .. text)
        show(n .. ":" .. index .. ":load", f and f())
      end
    end
  end
end
print(tonumber("0x" .. string.rep("1", 40) .. "p99999999999"), tonumber("0x1p-99999999999"))
