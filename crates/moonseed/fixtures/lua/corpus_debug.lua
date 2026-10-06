-- The debug library's introspection, printed for comparison with Lua 5.4.9
-- (Phase 3.24, ADR 0040). Run as a file named `corpus_debug.lua`. Lua has
-- a C level below the main chunk that Moonseed's host does not: tracebacks
-- drop it, and level walks stop at the main chunk.

local function tb(...)
  return (debug.traceback(...):gsub("\n\t%[C%]: in %?$", ""))
end

local function keys(t)
  local list = {}
  for k in pairs(t) do list[#list + 1] = k end
  table.sort(list)
  local out = {}
  for _, k in ipairs(list) do
    local v = t[k]
    if type(v) == "function" or type(v) == "table" then v = type(v) end
    out[#out + 1] = k .. "=" .. tostring(v)
  end
  return table.concat(out, " ")
end

local function lines(f)
  local t = {}
  for l in pairs(debug.getinfo(f, "L").activelines) do t[#t + 1] = l end
  table.sort(t)
  return table.concat(t, ",")
end

-- getinfo on functions
print("A1", keys(debug.getinfo(print)))
print("A2", keys(debug.getinfo(keys)))
print("A3", keys(debug.getinfo(lines, "S")))
print("A4", keys(debug.getinfo(function(...) end, "u")))
print("A5", keys(debug.getinfo(require, "u")))
print("A6", lines(keys), lines(tb))
print("A7", debug.getinfo(print, "L").activelines, debug.getinfo(1, "f").func ~= nil)
print("A8", pcall(debug.getinfo, 1, ">S"))
print("A9", pcall(debug.getinfo, 1, "Sx"))
print("A10", pcall(debug.getinfo, "x"))
print("A11", pcall(debug.getinfo, 1.5))
print("A12", type(debug.getinfo(1, "")), next(debug.getinfo(1, "")))
print("A13", debug.getinfo(-1), debug.getinfo(1000))
print("A14", keys(debug.getinfo(1, "Slnt")))

-- source names
print("B1", keys(debug.getinfo(load("return 1"), "S")))
print("B2", keys(debug.getinfo(load("return 1", "=name"), "S")))
print("B3", keys(debug.getinfo(load("return 1", "@some/file.lua"), "S")))
print("B4", keys(debug.getinfo(load("local x = 1\nreturn x", "a long chunk name that goes on and on past the sixty byte limit"), "S")))
print("B5", debug.getinfo(load("\n\nreturn debug.getinfo(1, 'l')"), "S").short_src)
print("B6", load("\n\nlocal i = debug.getinfo(1, 'l') return i.currentline")())
print("B7", debug.getinfo(load(function() return nil end), "S").source)

-- names from call sites
local results = {}
local function who(level)
  local i = debug.getinfo(level or 2, "n")
  results[#results + 1] = tostring(i.namewhat) .. ":" .. tostring(i.name)
end
function global_fn() who() end
local function local_fn() who() end
local obj = { m = function(self) who() end, f = function() who() end }
obj[1] = function() who() end
obj[300] = function() who() end
obj.long = obj.f
global_fn()
local_fn()
obj:m()
obj.m(obj)
obj.f()
obj[1]()
obj[300]()
obj["long"]()
local key = "f"
obj[key]()
;(local_fn)()
_ENV.global_fn()
local function up_caller() local_fn() end
up_caller()
for _ in function() who() end do end
local mt = {}
for _, event in ipairs({ "__index", "__newindex", "__add", "__sub", "__concat", "__eq", "__lt", "__le", "__len", "__unm", "__call", "__band", "__shl", "__bnot", "__idiv", "__mod", "__pow", "__div" }) do
  mt[event] = function() who() return 1 end
end
local a, b = setmetatable({}, mt), setmetatable({}, mt)
local _ = a.x
a.y = 1
_ = a + 1
_ = 1 - a
_ = a .. "s"
_ = a == b
_ = a < b
_ = a > b
_ = a <= b
_ = #a
_ = -a
_ = a & 1
_ = a << 1
_ = ~a
_ = a // 1
_ = a % 1
_ = a ^ 2
_ = a / 2
a()
do
  local c <close> = setmetatable({}, { __close = function() who() end })
end
pcall(who)
local s = tostring(setmetatable({}, { __tostring = function() who() return "" end }))
print("C1", table.concat(results, " "))
results = {}
local function tail_target() who() end
local function tail_from() return tail_target() end
tail_from()
print("C2", table.concat(results, " "), debug.getinfo(1, "t").istailcall)
local function tail_info() return debug.getinfo(1, "t").istailcall end
local function tail_to_info() return tail_info() end
print("C3", tail_to_info(), tail_info())

-- locals
local function locals(level)
  local out = {}
  for i = 1, 30 do
    local name, value = debug.getlocal(level + 1, i)
    if not name then break end
    if name:sub(1, 1) ~= "(" or name == "(for state)" then
      if type(value) == "function" or type(value) == "table" then value = type(value) end
      out[#out + 1] = name .. "=" .. tostring(value)
    end
  end
  return table.concat(out, " ")
end
local function params(x, y, ...)
  local z = x + y
  print("D1", locals(1))
  do
    local inner = "in"
    print("D2", locals(1))
  end
  print("D3", locals(1))
  print("D4", debug.getlocal(1, -1), debug.getlocal(1, -2), debug.getlocal(1, -3), debug.getlocal(1, -4))
  print("D5", debug.setlocal(1, -2, "changed"), select(2, ...))
  for i = 10, 11 do
    print("D6", locals(1))
  end
  for k, v in pairs({ "p" }) do
    print("D7", locals(1))
  end
  print("D8", debug.setlocal(1, 3, 99), z)
  print("D9", debug.setlocal(1, 50, 1), debug.getlocal(1, 50))
  return z
end
print("D10", params(1, 2, "v1", "v2", "v3"))
print("D11", debug.getlocal(params, 1), debug.getlocal(params, 2), debug.getlocal(params, 3), debug.getlocal(params, 0))
print("D12", debug.getlocal(print, 1), debug.getlocal(function() local q end, 1))
print("D13", pcall(debug.getlocal, 100, 1))
print("D14", pcall(debug.getlocal, 1))
print("D15", pcall(debug.setlocal, 1, 1))
print("D16", pcall(debug.setlocal, 100, 1, 1))
print("D17", pcall(debug.getlocal, 1, "x"))
local function varargs_none(...) return debug.getlocal(1, -1) end
print("D18", varargs_none())
local function no_varargs(a) return debug.getlocal(1, -1) end
print("D19", no_varargs(1))
local function caller_locals()
  local here = "caller"
  local function callee() local n, v = debug.getlocal(2, 1) return n, v end
  local n, v = callee()
  return n, v
end
print("D20", caller_locals())
local function late()
  local before = debug.getlocal(1, 1)
  local x = 5
  return before, (debug.getlocal(1, 1))
end
print("D21", late())
local function loop_break()
  local out = {}
  for i = 1, 3 do
    if i == 2 then
      debug.setlocal(1, 4, 100)
    end
    out[#out + 1] = i
  end
  return table.concat(out, ",")
end
print("D22", loop_break())

-- upvalues
local up1, up2 = 1, 2
local function uses() return up1 + up2 end
local function uses_global() return print, up1 end
print("E1", debug.getupvalue(uses, 1), debug.getupvalue(uses, 2), debug.getupvalue(uses, 3), debug.getupvalue(uses, 0))
print("E2", debug.getupvalue(uses_global, 1), debug.getupvalue(uses_global, 2))
print("E3", debug.setupvalue(uses, 1, 10), up1, uses())
print("E4", debug.setupvalue(uses, 5, 10), debug.getupvalue(print, 1))
print("E5", select("#", debug.getupvalue(print, 1)), select("#", debug.setupvalue(print, 1, 2)))
print("E6", select(1, debug.getupvalue(require, 1)) == "", select(2, debug.getupvalue(require, 1)) == package)
print("E7", pcall(debug.getupvalue, 1, 1))
print("E8", pcall(debug.getupvalue, uses))
print("E9", pcall(debug.setupvalue, uses, 1))
local function counter()
  local n = 0
  return function() n = n + 1 return n end
end
local c1, c2 = counter(), counter()
c1() c1() c2()
debug.upvaluejoin(c2, 1, c1, 1)
print("E10", c1(), c2(), c1())
local x1, x2 = "one", "two"
local function get1() return x1 end
local function get2() return x2 end
debug.upvaluejoin(get1, 1, get2, 1)
x2 = "three"
print("E11", get1(), get2(), x1)
print("E12", pcall(debug.upvaluejoin, get1, 2, get2, 1))
print("E13", pcall(debug.upvaluejoin, get1, 1, get2, 0))
print("E14", pcall(debug.upvaluejoin, print, 1, get2, 1))
print("E15", pcall(debug.upvaluejoin, require, 1, get2, 1))
print("E16", pcall(debug.upvaluejoin, get1, 1, require, 1))
print("E17", pcall(debug.upvaluejoin, get1, 1))
print("E18", pcall(debug.upvaluejoin, {}, 1, get2, 1))

-- metatables and the registry
print("F1", debug.getmetatable("").__index == string, debug.getmetatable(1), debug.getmetatable({}))
local protected = setmetatable({}, { __metatable = "locked" })
print("F2", getmetatable(protected), type(debug.getmetatable(protected)))
print("F3", debug.setmetatable(5, { __index = function(n, k) return k .. n end }) == 5, (7).foo, (1.5).bar)
print("F4", debug.getmetatable(3) == debug.getmetatable(4.5))
debug.setmetatable(5, nil)
print("F5", pcall(function() return (5).foo end) == false, debug.getmetatable(5))
debug.setmetatable(nil, { __index = function() return "from nil" end })
print("F6", (nil).anything)
debug.setmetatable(nil, nil)
debug.setmetatable(true, { __len = function(b) return b and 1 or 0 end, __call = function(b, x) return "called " .. tostring(x) end })
print("F7", #true, #false, (true)(9))
debug.setmetatable(true, nil)
debug.setmetatable(print, { __index = { kind = "function" } })
print("F8", print.kind, keys.kind)
debug.setmetatable(print, nil)
print("F9", pcall(debug.setmetatable, 1, 2))
print("F10", pcall(debug.setmetatable, 1))
print("F11", pcall(debug.getmetatable))
local t = debug.setmetatable({}, { __index = { z = 26 } })
print("F12", t.z, getmetatable(t).__index.z)
local reg = debug.getregistry()
print("F13", type(reg), reg == debug.getregistry(), reg._LOADED == package.loaded, reg._PRELOAD == package.preload)
print("F14", reg[2] == _G, type(reg[1]), rawequal(reg._LOADED.debug, debug))

-- tracebacks
local function level3() return tb("msg", 1) end
local function level2() local r = level3() return r end
local function level1() local r = level2() return r end
print("G1", level1())
print("G2", tb("m", 2))
print("G3", tb())
print("G4", tb(nil))
print("G5", type(debug.traceback({})), debug.traceback(true), debug.traceback(print) == print)
print("G6", tb(42))
print("G7", tb("x\0y"))
print("G8", tb("lvl", 0))
print("G9", tb("big", 50))
local function deep(n) if n == 0 then local r = tb("deep") return r end local r = deep(n - 1) return r end
-- Deeper stacks skip levels by counts that include Lua's C level.
print("G10", deep(17))
local function tail_tb(n) if n == 0 then local r = tb("t") return r end return tail_tb(n - 1) end
print("G13", tail_tb(3))
mymod = { run = function() local r = tb("mod") return r end }
package.loaded.mymod = mymod
print("G14", mymod.run())
package.loaded.mymod = nil
print("G15", select(2, pcall(string.rep)))
local ok, err = xpcall(function() local r = nil; return r.x end, function(m) return tb("handler") end)
print("G16", ok, err)
print("G17", pcall(debug.traceback, "m", "x"))
local co_like = setmetatable({}, { __index = function(t, k) return tb("meta") end })
print("G18", co_like.x)

-- stripped functions
local function stripme(p, q)
  local r = p + q
  print("H1", debug.getlocal(1, 1), debug.getlocal(1, 2))
  print("H2", debug.getinfo(1, "l").currentline, debug.getinfo(1, "S").short_src)
  return r
end
local stripped = load(string.dump(stripme, true))
print("H3", stripped(1, 2))
print("H4", keys(debug.getinfo(stripped, "S")), lines(stripped))
local captured = 5
local stripped_up = load(string.dump(function() return captured end, true))
print("H5", debug.getupvalue(stripped_up, 1) , type(select(2, debug.getupvalue(stripped_up, 1))))
local unstripped = load(string.dump(stripme))
print("H6", keys(debug.getinfo(unstripped, "S")), debug.getlocal(unstripped, 2))
print("H7", unstripped(3, 4))
local named = load(string.dump(load("return debug.getinfo(1, 'S').source", "=orig")))
print("H8", named())
gtb = tb
local stripped_names = load(string.dump(function() local r = gtb("s") return r end, true))
print("H9", stripped_names())

-- the main chunk
local main = debug.getinfo(1, "S")
print("I1", main.what, main.linedefined, main.lastlinedefined, main.source, main.short_src)
print("I2", keys(debug.getinfo(1, "u")))
print("I3", debug.getinfo(1, "l").currentline)

-- lines of multi-line calls, operators, and constructors
local chain = { m = function(self, x) return self, debug.getinfo(2, "l").currentline end }
local lines_seen = {}
local function note(_, line) lines_seen[#lines_seen + 1] = line return chain end
chain.m = function(self, x) return note(nil, debug.getinfo(2, "l").currentline) end
chain
  :m("a")
  :m("b")
  :m(
    "c")
print("J1", table.concat(lines_seen, ","))
local seen = {}
local mm = setmetatable({}, { __add = function() seen[#seen + 1] = debug.getinfo(2, "l").currentline return 0 end,
  __concat = function() seen[#seen + 1] = debug.getinfo(2, "l").currentline return "" end,
  __unm = function() seen[#seen + 1] = debug.getinfo(2, "l").currentline return 0 end })
local r1 = mm
  +
  1
local r2 = mm ..
  "x"
local r3 =
  - mm
print("J2", table.concat(seen, ","))
local function ctor()
  local t = {
    a = 1,
    b = 2,
  }
  return t
end
print("J3", lines(ctor))
print("done")
