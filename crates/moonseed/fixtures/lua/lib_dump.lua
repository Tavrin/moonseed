-- string.dump and binary load (ADR 0036). Output must match Lua 5.4.9: the
-- bytes differ, what the loaded functions do does not.
local function show(...)
  local out = {}
  for i = 1, select('#', ...) do
    local v = select(i, ...)
    -- `%q` keeps bytes past 127 as they are; write them as escapes.
    out[#out + 1] = type(v) == "string"
        and (string.gsub(string.format("%q", v), "[\128-\255]", function(c) return "\\" .. c:byte() end))
      or tostring(v)
  end
  print(table.concat(out, " "))
end
local f = function(a, ...) return a, ... end
local d = string.dump(f)
local g = assert(load(d, nil, "b"))
show(type(d), d:byte(1), g(1, 2, nil, 4))
show(select("#", g(1, 2, nil, 4)), g ~= f)
local x, y, z = 10, 20, 30
local function h() return x, y, z end
local h2 = load(string.dump(h))
show(rawequal(h2(), _G), select(2, h2()), select(3, h2()))
local env = {}
local h3 = load(string.dump(h), "c", "b", env)
show(rawequal(h3(), env), select(2, h3()))
local function envf() return print ~= nil, _ENV == _G end
show(load(string.dump(envf))())
show(load(string.dump(envf), "c", "bt", { print = 1 })())
show(pcall(string.dump, print))
show(pcall(string.dump, 1))
show(pcall(string.dump))
show(load(string.dump(f), "x", "t"))
show(load("return 1", "x", "b"))
show(load("return 1", "x", "bt")())
local nested = function() local a = 1 return function() a = a + 1 return a end end
local n2 = load(string.dump(nested))()
show(n2(), n2(), n2())
local s1, s2 = string.dump(f), string.dump(f, true)
show(load(s2)(5, 6), load(s1, "c", "b")(7), string.dump(f) == string.dump(f))
local counter = 0
local function recursive(n) if n == 0 then return 0 end return n + recursive(n - 1) end
show((pcall(load(string.dump(recursive)), 3)))
local pieces = { string.dump(function(...) return select("#", ...) end) }
local i = 0
local fromreader = load(function() i = i + 1 return pieces[i] end, "r", "b")
show(fromreader(1, 2, 3))
show(load(function() return nil end, "r", "b"))
local up = load(string.dump(function() counter = counter + 1 return counter end))
show((pcall(up)))
return "done"
