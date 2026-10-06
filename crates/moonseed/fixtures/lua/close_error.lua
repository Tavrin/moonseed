local log = {}
local n = 0
local push = function(v) n = n + 1 log[n] = v end
local mk = function(name, fail)
  return setmetatable({}, { __close = function(self, err)
    push(name)
    push(err)
    if fail ~= nil then error(fail, 0) end
  end })
end
local ok1, e1 = pcall(function()
  local a <close> = mk("a")
  local b <close> = mk("b", "eb")
  local c <close> = mk("c", "ec")
  error("orig", 0)
end)
local ok2, e2 = pcall(function()
  local a <close> = mk("a2")
  local b <close> = mk("b2", "eb2")
  local c <close> = mk("c2")
end)
local ok3 = pcall(function() local x <close> = {} end)
local ok4 = pcall(function() local x <close> = setmetatable({}, { __close = 42 }) end)
local ok5, e5 = pcall(function()
  local a <close> = mk("a5")
  local b <close> = {}
end)
return ok1, e1, ok2, e2, ok3, ok4, ok5, n, log[1], log[2], log[3], log[4], log[5], log[6], log[7], log[8], log[9], log[10], log[11], log[12], log[13]
