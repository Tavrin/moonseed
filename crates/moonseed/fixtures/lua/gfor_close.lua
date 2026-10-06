local log = {}
local n = 0
local put = function(v) n = n + 1 log[n] = v end
local mk = function(name)
  return setmetatable({}, { __close = function(self, err)
    put(name)
    if err ~= nil then put(err) end
  end })
end
local upto2 = function(s, c)
  if c == nil then return 1 end
  if c < s then return c + 1 end
end
for i in upto2, 2, nil, mk("H1") do
  local b <close> = mk("b" .. i)
end
local calls = 0
local forever = function() calls = calls + 1 return 1 end
for x in forever, nil, nil, mk("H2") do
  local b <close> = mk("bb")
  break
end
for i in upto2, 2, nil, mk("O") do
  for j in forever, nil, nil, mk("I" .. i) do
    break
  end
  put("after" .. i)
end
local f = function()
  local outer <close> = mk("F")
  for i in upto2, 5, nil, mk("H3") do
    local b <close> = mk("r" .. i)
    if i == 2 then return i * 100 end
  end
end
local r = f()
local plain = 0
for i in upto2, 2, nil, false do plain = plain + i end
for i in upto2, 2, nil, nil do plain = plain + i end
local called = 0
local spy = function() called = called + 1 end
local ok = pcall(function() for x in spy, nil, nil, {} do end end)
local swap = setmetatable({}, { __close = function() put("old") end })
for i in upto2, 1, nil, swap do
  getmetatable(swap).__close = function() put("new") end
end
return calls, r, plain, ok, called, n, log[1], log[2], log[3], log[4], log[5], log[6], log[7], log[8], log[9], log[10], log[11], log[12], log[13], log[14], log[15]
