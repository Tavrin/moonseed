local log = {}
local n = 0
local mk = function(name)
  return setmetatable({}, { __close = function(self, err)
    n = n + 1
    log[n] = name
    if err ~= nil then n = n + 1 log[n] = err end
  end })
end
do
  local a <close> = mk("a")
  local b <close> = mk("b")
  local c <close> = mk("c")
end
do
  local x <close> = nil
  local y <close> = false
  local z <close> = mk("z")
end
while true do
  local w <close> = mk("w")
  break
end
for i = 1, 2 do
  local f <close> = mk("f")
end
local r = function()
  local q <close> = mk("q")
  return 1, 2
end
local r1, r2 = r()
local keep = function()
  local k <close> = mk("k")
  return k
end
local kept = keep()
repeat
  local u <close> = mk("u")
until u ~= nil
return n, log[1], log[2], log[3], log[4], log[5], log[6], log[7], log[8], log[9], log[10], r1, r2, getmetatable(kept) ~= nil
