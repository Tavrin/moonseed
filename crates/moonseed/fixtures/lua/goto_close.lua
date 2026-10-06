local show = function(v)
  if v == nil then return "nil" end
  if v == true then return "true" end
  if v == false then return "false" end
  return v
end
local line = ""
local put = function(...)
  local k = select('#', ...)
  local s = "(" .. k .. ")"
  for i = 1, k do s = s .. " " .. show((select(i, ...))) end
  line = line .. s .. ";"
end
local log = ""
local mk = function(t) return setmetatable({}, { __close = function() log = log .. t end }) end
local n = 0
::again::
do
  local v <close> = mk("c" .. n)
  n = n + 1
  if n < 3 then goto again end
end
put(log)
log = ""
local done = false
::out::
if not done then
  done = true
  do
    local a <close> = mk("a")
    local up = 1
    local f = function() return up end
    do
      local b <close> = mk("b")
      do
        local c <close> = mk("c")
        goto out
      end
    end
  end
end
put(log)
log = ""
local bad = function(t) return setmetatable({}, { __close = function() log = log .. t error("E" .. t, 0) end }) end
local ok, e = pcall(function()
  do
    local a <close> = mk("a")
    local b <close> = bad("b")
    goto after
  end
  ::after::
  log = log .. "!"
end)
put(ok, e, log)
log = ""
local closed = 0
local it = function(s, c) if c < 3 then return c + 1 end end
local first = true
::outer::
if first then
  first = false
  for x in it, nil, 0, setmetatable({}, { __close = function() closed = closed + 1 end }) do
    goto outer
  end
end
local stays = 0
for x in it, nil, 0, setmetatable({}, { __close = function() stays = stays + 1 end }) do
  if x < 3 then goto continue end
  ::continue::
end
put(closed, stays)
log = ""
local s = 0
for k in upto, 4 do
  do
    local c <close> = mk(k)
    if k == 2 then goto next end
    s = s + k
  end
  ::next::
end
put(s, log)
return line
