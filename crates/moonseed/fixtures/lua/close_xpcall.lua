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
local h = function(m) push("h") push(m) return "H" .. m end
local ok1, e1 = xpcall(function() local a <close> = mk("a") error("orig", 0) end, h)
local ok2, e2 = xpcall(function()
  local a <close> = mk("a2")
  local b <close> = mk("b2", "eb")
  error("o2", 0)
end, h)
local ok3, e3 = xpcall(function()
  local a <close> = mk("a3")
  local b <close> = mk("b3", "eb3")
end, h)
return ok1, e1, ok2, e2, ok3, e3, n, log[1], log[2], log[3], log[4], log[5], log[6], log[7], log[8], log[9], log[10], log[11], log[12], log[13], log[14], log[15], log[16], log[17], log[18]
