local log = {}
local n = 0
local put = function(v) n = n + 1 log[n] = v end
local mk = function(name)
  return setmetatable({}, { __close = function(self, err)
    put(name)
    put(err)
  end })
end
local bodies = 0
local ok1, e1 = pcall(function()
  for x in function() error("boom", 0) end, nil, nil, mk("H") do
    bodies = bodies + 1
  end
end)
local t = {}
local ok2, e2 = pcall(function()
  local o <close> = mk("O")
  for x in function() return 1 end, nil, nil, mk("H2") do
    local b <close> = mk("B")
    error(t)
  end
end)
local ok3, e3 = pcall(function()
  local o <close> = mk("O3")
  for x in function() error("it", 0) end, nil, nil,
      setmetatable({}, { __close = function(self, err) put("C") put(err) error("ce", 0) end }) do
  end
end)
local ok4, e4 = xpcall(function()
  for x in function() error("x4", 0) end, nil, nil, mk("H4") do end
end, function(m) put("h") put(m) return "H" .. m end)
return ok1, e1, bodies, ok2, e2 == t, ok3, e3, ok4, e4, n, log[1], log[2], log[3], log[4] == t, log[5], log[6] == t, log[7], log[8] == t, log[9], log[10], log[11], log[12], log[13], log[14], log[15], log[16]
