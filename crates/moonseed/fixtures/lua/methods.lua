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
local three = function() return 10, nil, 30 end
local count = 0
local make = function()
  count = count + 1
  return { value = 42, get = function(self) return self.value end }
end
local result = make():get()
put(count, result)
local Class = {}
Class.__index = Class
function Class.new(v) return setmetatable({ v = v }, Class) end
function Class:get() return self.v end
function Class:add(n, ...) return self.v + n, select('#', ...) end
local o = Class.new(5)
put(o:get(), o:add(3), o:add(1, nil, nil))
local proxy = setmetatable({}, { __index = function(t, k) return function(self, x) return k .. x, self == t end end })
put(proxy:hello("!"))
local callable = setmetatable({}, { __call = function(c, self, a) return self == o, a end })
o.call = callable
put(o:call(9))
local s = {}
function s:t(tbl) return #tbl end
function s:str(x) return x .. "?" end
put(s:t{ 1, 2, 3 }, s:str"q", s:str[[long]])
local id = function(x) return x end
put(id"lit", id{ 7 }[1], #id{ 1, 2 })
function s:multi(...) return ... end
put(s:multi(1, nil, 3))
put(s:multi(three()))
put(s:multi(three()), 4)
local a = { b = { c = {} } }
function a.b.c.f(x) return x * 2 end
function a.b.c:m(x) return self == a.b.c, x end
put(a.b.c.f(21), a.b.c:m(1))
function gf(x) return x + 1 end
local lf
function lf(x) return x + 2 end
put(gf(1), lf(1), rawget(_ENV, "lf") == nil, rawget(_ENV, "gf") == gf)
local function fact(k) if k == 0 then return 1 end return k * fact(k - 1) end
put(fact(10))
local f = 10
do
  local function f(k) if k == 0 then return "inner" end return f(k - 1) end
  put(f(3))
end
put(f)
local g = "outer"
local g = function(k) return g end
put(g(1))
local seen
local tt = setmetatable({}, { __newindex = function(self, key, value) seen = key rawset(self, key, value) end })
function tt.answer() return 42 end
local first = seen
function tt:meth() return self == tt end
put(first, seen, tt.answer(), tt:meth())
local lookups = 0
local auto
auto = { __index = function(t, k) lookups = lookups + 1 local v = setmetatable({}, auto) rawset(t, k, v) return v end }
local root = setmetatable({}, auto)
function root.x.y.z() return "deep" end
put(lookups, root.x.y.z())
function s:va(p, ...) return self == s, p, select('#', ...), ... end
put(s:va(1, 2, nil))
local counter = { n = 0 }
function counter:inc() self.n = self.n + 1 return self end
counter:inc():inc():inc()
put(counter.n)
local ok, e = pcall(function() local z = nil return z:nope() end)
put(ok)
local ok2 = pcall(function() local z = {} return z:nope() end)
put(ok2)
local arr = { { m = function(self, x) return x or 5 end } }
local i, a = 0, 1
local flags = { [true] = { m = function(self) return 1 end } }
put(arr[i + 1]:m(), arr[i * 2 + 1]:m(7), flags[a == 1]:m())
local other = { tag = "O" }
local r
r = setmetatable({ tag = "A" }, { __index = function(t, k) r = other return function(self) return self.tag end end })
local first_tag = r:m()
r = setmetatable({ tag = "B" }, { __index = function(t, k) r = other return function(self) return self.tag end end })
local tail = function() return r:m() end
put(first_tag, tail())
return line
