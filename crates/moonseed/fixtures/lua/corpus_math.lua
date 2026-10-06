-- A generated corpus of math calls (ADR 0032): every function on every
-- value kind, and pairs for the binary ones. Compared with Lua 5.4.9.
local function show(v)
  if type(v) == "number" and v ~= v then return "nan" end
  return tostring(v) .. ":" .. (math.type(v) or type(v))
end
local values = { 0, 1, -1, 2, 3, -7, 9007199254740992, 9007199254740993, math.maxinteger, math.mininteger,
  0.0, -0.0, 0.5, -0.5, 1.5, -2.5, 3.0, 2^53, 2^63, -2^63, 1e308, -1e308, 5e-324, 1/0, -1/0, 0/0, 0.1, 100.0, 1e-10,
  "3.5", " 0x10 ", "10", "-4", "abc", "", nil, true, {} }
local unary = { "abs", "ceil", "floor", "sqrt", "sin", "cos", "tan", "asin", "acos", "atan", "exp", "log",
  "deg", "rad", "tointeger", "type", "modf" }
local binary = { "fmod", "atan", "log", "ult", "min", "max" }
local count = 38 -- `values` holds a nil, so `#` is not its size
for _, name in ipairs(unary) do
  local f = math[name]
  for i = 1, count do
    local ok, a, b = pcall(f, values[i])
    if ok then print(name, i, show(a), b ~= nil and show(b) or "") else print(name, i, "error") end
  end
  print(name, "none", (pcall(f)))
end
local pick = { 1, 2, 3, 5, 6, 8, 10, 11, 12, 13, 15, 18, 22, 24, 25, 26, 30, 32, 34, 37 }
for _, name in ipairs(binary) do
  local f = math[name]
  for _, i in ipairs(pick) do
    for _, j in ipairs(pick) do
      local ok, a = pcall(f, values[i], values[j])
      print(name, i, j, ok and show(a) or "error")
    end
  end
end
return "done"
