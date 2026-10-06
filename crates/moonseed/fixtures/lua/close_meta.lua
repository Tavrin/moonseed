local log = {}
local n = 0
local push = function(v) n = n + 1 log[n] = v end
do
  local mt = { __close = function() push("first") end }
  local x <close> = setmetatable({}, mt)
  mt.__close = function() push("second") end
end
local ok = pcall(function()
  local mt = { __close = function() push("never") end }
  local x <close> = setmetatable({}, mt)
  mt.__close = nil
end)
do
  local callable = setmetatable({}, { __call = function(self, v, err) push("called") push(err == nil) end })
  local y <close> = setmetatable({}, { __close = callable })
end
local get
local seen
do
  local x <close> = setmetatable({ v = 1 }, { __close = function(self) seen = get() self.v = 2 end })
  get = function() return x end
  x.v = 5
end
return ok, n, log[1], log[2], log[3], seen.v, get().v, rawequal(seen, get())
