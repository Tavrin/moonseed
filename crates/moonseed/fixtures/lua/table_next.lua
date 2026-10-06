local t = {}
t[10] = true
t[20] = true
t[30] = true
local first = next(t)
t[first] = nil
local rest = {}
local key = next(t, first)
while key do
  rest[#rest + 1] = key
  key = next(t, key)
end
local function has(wanted)
  for index = 1, #rest do
    if rest[index] == wanted then
      return true
    end
  end
  return false
end
assert(first ~= nil)
assert(not has(first))
assert(#rest == 2)
local ok, _ = pcall(function()
  next({}, 1)
end)
assert(not ok)
io.write("ok\n")
