-- Userdata corpus (Phase 3.26, ADR 0042 to ADR 0045). Needs the host's
-- newud(size [, nuv]), light(n), udpeek(u, i), and udpoke(u, i, b): Lua
-- source cannot make userdata. Addresses are never printed: Lua prints
-- pointers, Moonseed object ids.

local names = setmetatable({}, {__mode = nil})
local function name(v, n) names[v] = n return v end
local function fmt(v)
  if type(v) == "userdata" then return "<" .. (names[v] or "?") .. ">" end
  return tostring(v)
end
local function show(...)
  local out = {select("#", ...)}
  for i = 1, select("#", ...) do out[#out + 1] = fmt((select(i, ...))) end
  print(table.concat(out, " "))
end
local function try(f, ...)
  return show(pcall(f, ...))
end

-- A. Types and identity.
print("A")
local u1 = name(newud(4, 2), "u1")
local u2 = name(newud(4, 2), "u2")
local l1 = name(light(1), "l1")
show(type(u1), type(l1), math.type(u1), select("#", u1))
show(u1 == u1, u1 == u2, rawequal(u1, u1), rawequal(u1, u2))
show(l1 == light(1), l1 == light(2), rawequal(l1, light(1)), l1 == u1)
show(tostring(u1):match("^userdata: ") ~= nil, tostring(l1):match("^userdata: ") ~= nil)
show(tostring(u1) == tostring(u1), tostring(u1) ~= tostring(u2))
show(tostring(light(7)) == tostring(light(7)), tostring(light(7)) ~= tostring(light(8)))
show(string.format("%p", u1) == tostring(u1):match(": (.*)$"))
show(string.format("%p", l1) == tostring(l1):match(": (.*)$"))
show(string.format("%p", u1) ~= string.format("%p", u2))
local z = newud(0)
show(type(z), debug.getuservalue(z))
local v = u1
show(v == u1, v ~= u2)

-- B. Byte payloads: zeroed, kept, separate.
print("B")
local b = newud(8)
show(udpeek(b, 0), udpeek(b, 7))
udpoke(b, 3, 200)
show(udpeek(b, 3), udpeek(newud(8), 3))

-- C. User values.
print("C")
local uv = name(newud(0, 3), "uv")
show(debug.getuservalue(uv))
show(debug.getuservalue(uv, 1))
show(debug.getuservalue(uv, 3))
show(debug.getuservalue(uv, 0))
show(debug.getuservalue(uv, 4))
show(debug.getuservalue(uv, -1))
show(debug.getuservalue(l1))
show(debug.getuservalue({}))
show(debug.getuservalue(nil))
show(debug.getuservalue(1, 1))
show(debug.setuservalue(uv, "one"))
show(debug.setuservalue(uv, {}, 2))
show(debug.setuservalue(uv, uv, 3))
show(debug.setuservalue(uv, 4, 4))
show(debug.setuservalue(uv, 0, 0))
show(debug.getuservalue(uv, 1))
show(type(debug.getuservalue(uv, 2)), debug.getuservalue(uv, 3) == uv)
show(debug.getuservalue(uv, 2^32 + 1))
show(debug.setuservalue(uv, nil, 1))
show(debug.getuservalue(uv, 1))
try(debug.setuservalue, l1, 1)
try(debug.setuservalue, {}, 1)
try(debug.setuservalue, uv)
try(debug.setuservalue)
try(debug.getuservalue, uv, "x")
try(debug.getuservalue, uv, 1.5)
try(debug.setuservalue, uv, 1, "x")
show(debug.getuservalue(newud(0, 0), 1))

-- D. Metatables.
print("D")
show(getmetatable(u1), debug.getmetatable(u1))
local mt = {__name = "Point"}
show(debug.setmetatable(u1, mt) == u1, getmetatable(u1) == mt, getmetatable(u2))
show(tostring(u1):match("^Point: ") ~= nil, string.format("%s", u1):match("^Point: ") ~= nil)
debug.setmetatable(u2, mt)
show(getmetatable(u2) == getmetatable(u1))
local mt2 = {}
debug.setmetatable(u2, mt2)
show(getmetatable(u2) == mt2, getmetatable(u1) == mt)
mt.__metatable = "locked"
show(getmetatable(u1), debug.getmetatable(u1) == mt)
mt.__metatable = nil
try(setmetatable, u1, {})
try(setmetatable, {})
try(rawget, {})
try(rawset, {}, 1)
try(rawset, {})
try(setmetatable, l1, {})
show(debug.setmetatable(u1, nil) == u1, getmetatable(u1))
try(debug.setmetatable, u1, 5)
-- Light userdata share one metatable, as in Lua.
local lmt = {__name = "Light"}
show(debug.setmetatable(light(1), lmt) == light(1))
show(getmetatable(light(2)) == lmt, getmetatable(l1) == lmt, getmetatable(u2) == mt2)
show(tostring(light(3)):match("^Light: ") ~= nil)
show(getmetatable(newud(1)))
debug.setmetatable(light(9), nil)
show(getmetatable(l1))
-- __name names a value in argument errors; a light one is "light userdata".
local named = debug.setmetatable(newud(1), {__name = "Thing"})
try(string.rep, named)
try(string.rep, l1)
try(string.rep, u2)
try(math.floor, named)
try(ipairs)

-- E. Every event on full userdata, from one shared metatable.
print("E")
local log = {}
local ops = {}
for _, e in ipairs({"add", "sub", "mul", "div", "mod", "pow", "idiv",
    "band", "bor", "bxor", "shl", "shr", "concat"}) do
  ops["__" .. e] = function(a, b) return e .. "(" .. fmt(a) .. "," .. fmt(b) .. ")" end
end
ops.__unm = function(a, b) return "unm(" .. fmt(a) .. "," .. fmt(b) .. ")" end
ops.__bnot = function(a, b) return "bnot(" .. fmt(a) .. "," .. fmt(b) .. ")" end
ops.__len = function(a) return 42 end
ops.__lt = function(a, b) log[#log + 1] = "lt" return 1 end
ops.__le = function(a, b) log[#log + 1] = "le" return nil end
ops.__call = function(self, ...) return "called", fmt(self), ... end
ops.__index = function(self, k) return "get:" .. tostring(k) end
ops.__newindex = function(self, k, v) log[#log + 1] = "set:" .. tostring(k) .. "=" .. tostring(v) end
ops.__tostring = function(self) return "T(" .. fmt(self) .. ")" end
local ea = name(debug.setmetatable(newud(0), ops), "ea")
local eb = name(debug.setmetatable(newud(0), ops), "eb")
show(ea + 1, 1 + ea, ea - eb, ea * 2, ea / 2, ea % 2, ea ^ 2, ea // 2)
show(ea & 1, 1 | ea, ea ~ 1, ea << 1, 1 >> ea, ~ea, -ea)
show(ea .. "s", "s" .. ea, 1 .. ea, #ea)
show(ea < eb, ea <= eb, ea > 1, 1 >= ea)
show(table.concat(log, " "))
log = {}
show(ea(1, nil, 3))
show(ea.key, ea[1], ea[ea] == "get:T(<ea>)")
ea.x = 5
ea[1] = nil
show(table.concat(log, " "))
show(tostring(ea), string.format("%s|%5.3s", ea, eb))
-- __index and __newindex as tables.
local store = {}
local tab = name(debug.setmetatable(newud(0), {__index = {a = 1, b = 2}, __newindex = store}), "tab")
tab.c = 3
show(tab.a, tab.b, tab.c, store.c, rawget(store, "c"))
-- An __index chain through another userdata.
local inner = debug.setmetatable(newud(0), {__index = function(_, k) return "inner " .. k end})
local outer = debug.setmetatable(newud(0), {__index = inner})
show(outer.deep)
-- No metamethod: errors, not crashes.
local bare = newud(0)
log = {}
for _, f in ipairs({
  function() return bare.x end, function() bare.x = 1 end, function() return #bare end,
  function() return bare + 1 end, function() return bare < bare end,
  function() return bare .. "" end, function() return bare() end, function() return -bare end,
  function() return ~bare end, function() return bare <= 1 end,
}) do log[#log + 1] = tostring((pcall(f))) end
print(table.concat(log, " "))
log = {}
show(bare == bare, bare ~= newud(0))

-- F. __eq: full userdata ask; light userdata never do.
print("F")
local calls = 0
local eqt = {__eq = function(a, b) calls = calls + 1 return "yes" end}
local p = debug.setmetatable(newud(0), eqt)
local q = debug.setmetatable(newud(0), eqt)
local r = newud(0)
show(p == q, p ~= q, p == p, calls)
show(p == r, r == p, calls)
show(r == r, r == newud(0), calls)
local lhs = debug.setmetatable(newud(0), {__eq = function() return false end})
show(lhs == p, p == lhs, calls)
show(p == {}, p == 1, p == l1, calls)
local noneq = debug.setmetatable(newud(0), {__eq = function() return nil end})
show(noneq == q, q == noneq, calls)
show(rawequal(p, q), rawequal(p, p))
debug.setmetatable(light(0), {__eq = function() calls = calls + 100 return true end,
  __index = function(_, k) return "light " .. k end,
  __add = function(a, b) return "ladd" end,
  __len = function() return 7 end,
  __call = function(self, x) return "lcall", x end,
  __lt = function() return true end,
  __concat = function() return "lcat" end})
show(light(1) == light(2), light(1) ~= light(2), light(1) == light(1), calls)
show(light(1).field, light(2) + 1, #light(3), light(4)(9), light(5) < light(6), light(1) .. "")
show(light(1) == p, p == light(1), calls)
debug.setmetatable(light(0), nil)

-- G. Table keys.
print("G")
local t = {}
local k1, k2 = newud(0), newud(0)
t[k1] = "k1"; t[k2] = "k2"; t[light(1)] = "l1"; t[light(2)] = "l2"
show(t[k1], t[k2], t[light(1)], t[light(2)], t[light(3)], t[newud(0)])
show(rawget(t, k1), rawget(t, light(2)))
rawset(t, light(3), "l3")
show(t[light(3)])
local order = {}
for k, v in pairs(t) do order[#order + 1] = v end
show(#order)
t[k1] = nil; t[light(1)] = nil
local seen = {}
for k, v in next, t do seen[#seen + 1] = v end
table.sort(seen)
show(table.concat(seen, ","))
t[k1] = "again"; t[light(1)] = "again l"
show(t[k1], t[light(1)])
local keyed = debug.setmetatable(newud(0), {__eq = function() return true end})
local kt = {[keyed] = 1}
show(kt[keyed], kt[debug.setmetatable(newud(0), getmetatable(keyed))])
-- Userdata as keys and values through table functions.
local list = {newud(0), light(1), newud(0)}
show(#list, type(list[2]), list[2] == light(1))
table.insert(list, 1, light(5))
show(list[1] == light(5), #list)

-- H. debug.upvalueid.
print("H")
local function counter()
  local n = 0
  local function inc() n = n + 1 return n end
  local function get() return n end
  return inc, get
end
local inc1, get1 = counter()
local inc2, get2 = counter()
local id1 = debug.upvalueid(inc1, 1)
show(type(id1), id1 == debug.upvalueid(get1, 1), id1 == debug.upvalueid(inc2, 1))
show(debug.upvalueid(get2, 1) == debug.upvalueid(inc2, 1))
inc1()
show(id1 == debug.upvalueid(inc1, 1))
show(debug.upvalueid(inc1, 2), debug.upvalueid(inc1, 0), debug.upvalueid(inc1, -1))
show(debug.upvalueid(print, 1))
show(debug.upvalueid(function() end, 1))
local x, y = 1, 1
local fx = function() return x end
local fy = function() return y end
show(debug.upvalueid(fx, 1) == debug.upvalueid(fy, 1))
debug.upvaluejoin(fx, 1, fy, 1)
show(debug.upvalueid(fx, 1) == debug.upvalueid(fy, 1), fx())
local ids = {}
for i = 1, 3 do
  local cell = i
  ids[i] = debug.upvalueid(function() return cell end, 1)
end
show(ids[1] == ids[2], ids[2] == ids[3])
local open
do
  local held = 5
  local f = function() return held end
  open = debug.upvalueid(f, 1)
  local g = function() held = held + 1 end
  show(open == debug.upvalueid(g, 1))
  closed_f = f
end
show(open == debug.upvalueid(closed_f, 1), closed_f())
local idkeys = {[id1] = "cell"}
show(idkeys[debug.upvalueid(get1, 1)], idkeys[debug.upvalueid(get2, 1)])
try(debug.upvalueid, print)
try(debug.upvalueid, 1, 1)
try(debug.upvalueid)
try(debug.upvalueid, inc1, "x")
show(debug.getmetatable(id1))
closed_f = nil

-- I. <close> userdata.
print("I")
local closes = {}
local cmt = {__close = function(self, err) closes[#closes + 1] = fmt(self) .. ":" .. tostring(err) end}
local function closer(n) return name(debug.setmetatable(newud(0), cmt), n) end
do
  local a <close> = closer("scope")
end
local function ret()
  local a <close> = closer("ret")
  return "r"
end
show(ret())
do
  local i = 0
  ::top::
  i = i + 1
  do
    local a <close> = closer("goto" .. i)
    if i < 2 then goto top end
  end
end
show(pcall(function()
  local a <close> = closer("err")
  error("boom", 0)
end))
local co = coroutine.create(function()
  local a <close> = closer("co")
  coroutine.yield(1)
end)
coroutine.resume(co)
show(coroutine.close(co))
local ymt = {__close = function(self) closes[#closes + 1] = "yield-close" coroutine.yield("in close") end}
local yco = coroutine.wrap(function()
  local a <close> = debug.setmetatable(newud(0), ymt)
  return "done"
end)
show(yco(), yco())
show(table.concat(closes, " "))
show((pcall(function() local a <close> = newud(0) end)))
show((pcall(function() local a <close> = light(1) end)))

-- J. Failing metamethods, coroutines, and libraries.
print("J")
local bad = debug.setmetatable(newud(0), {
  __index = function() error("idx", 0) end,
  __add = function() error({}, 0) end,
  __call = function(_, n) if n then return n * 2 end error("call", 0) end,
})
show(pcall(function() return bad.x end))
show(select("#", pcall(function() return bad + 1 end)), type(select(2, pcall(function() return bad + 1 end))))
show(pcall(bad), pcall(bad, 4))
local ymeta = debug.setmetatable(newud(0), {
  __index = function(_, k) return coroutine.yield("index " .. k) end,
  __lt = function() return coroutine.yield("lt") end,
  __call = function(_, a) return coroutine.yield("call " .. a) end,
  __concat = function() return coroutine.yield("concat") end,
})
local yc = coroutine.wrap(function()
  local a = ymeta.key
  local b = ymeta < ymeta
  local c = ymeta(3)
  local d = ymeta .. "x"
  return "end", a, b, c, d
end)
show(yc())
show(yc("A"))
show(yc(false))
show(yc("C"))
show(yc("D"))
-- pairs and ipairs through metamethods.
local pud = debug.setmetatable(newud(0), {
  __pairs = function(self) return function(_, k) if not k then return 1, "one" end end, self, nil end,
})
for k, v in pairs(pud) do show(k, v) end
local iud = debug.setmetatable(newud(0), {__index = function(_, i) if i <= 3 then return i * 10 end end})
local walked = {}
for i, v in ipairs(iud) do walked[#walked + 1] = i .. "=" .. v end
print(table.concat(walked, " "))
show(select("#", pairs(newud(0))), select(2, pairs(l1)) == l1, select(3, pairs(l1)))
try(next, newud(0))
try(rawlen, newud(4))
try(rawget, newud(0), 1)
try(rawset, newud(0), 1, 1)
show(rawequal(newud(0), light(1)))
-- table functions on a userdata that acts as a table.
local backing = {}
local tud = debug.setmetatable(newud(0), {
  __index = backing, __newindex = backing, __len = function() return #backing end,
})
table.insert(tud, "a"); table.insert(tud, "b"); table.insert(tud, 1, "z")
show(#tud, table.concat(tud, ","), table.unpack(tud))
show(table.remove(tud), #tud)
table.sort(tud)
show(table.concat(tud, ","))
try(table.insert, newud(0), 1)
show(select("#", table.unpack({newud(0), light(1)})))
-- tostring with a bad __tostring, and __name not a string.
try(tostring, debug.setmetatable(newud(0), {__tostring = function() return 1 end}))
show(tostring(debug.setmetatable(newud(0), {__name = 5})):match("^userdata: ") ~= nil)
-- A userdata error object.
local errud = name(newud(0), "errud")
show(pcall(error, errud))
print("end")
