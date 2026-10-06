-- Generational collection from Lua (Phase 3.29, ADR 0051). What each part
-- prints is decided by what is reachable, never by when collections run,
-- so Lua 5.4.9 prints the same in either mode it starts in.

local function count(t)
  local n = 0
  for _ in pairs(t) do n = n + 1 end
  return n
end
local log = {}
local function flush(tag)
  table.sort(log)
  print(tag, #log, table.concat(log, " "))
  log = {}
end
local function fin(name)
  return {__gc = function() log[#log + 1] = name end}
end

-- A. Modes: each change returns the mode before it; 0 leaves a
-- multiplier as it is; a young collection's step ends no cycle.
collectgarbage("generational")
print("A", collectgarbage("generational"), collectgarbage("generational", 0, 0))
print("A", collectgarbage("incremental"), collectgarbage("generational", 25, 150))
print("A", collectgarbage("generational", 20, 100), collectgarbage("isrunning"))
collectgarbage()
print("A step", collectgarbage("step", 0), collectgarbage("step", 0))
print("A collect", collectgarbage("collect"), collectgarbage("generational"))

-- B. An old table given a new one keeps it through young collections
-- (gengc.lua's table barrier).
do
  local U = {}
  collectgarbage()
  U[1] = {x = {234}}
  collectgarbage("step", 0)
  collectgarbage("step", 0)
  collectgarbage("step", 0)
  print("B", U[1].x[1])
end

-- C. A new metatable of an old table, then given a finalizer (gengc.lua).
do
  local old = {10}
  collectgarbage()
  setmetatable(old, {})
  collectgarbage("step", 0)
  setmetatable(getmetatable(old), {__gc = function() end})
  collectgarbage("step", 0)
  print("C", old[1])
end

-- D. A finalized object anchored by its finalizer in an old table keeps
-- its metatable (gengc.lua, a 5.4.0 bug).
do
  local A = {}
  A[1] = false
  local function gcf(obj)
    A[1] = obj
    obj = nil
    collectgarbage("step", 0)
    log[#log + 1] = getmetatable(A[1]).x
  end
  collectgarbage()
  local obj = {}
  collectgarbage("step", 0)
  setmetatable(obj, {__gc = gcf, x = "+"})
  obj = nil
  collectgarbage("step", 0)
  collectgarbage("step", 0)
  flush("D")
end

-- E. A closure over a coroutine's local, the local given a new table by
-- a resume, an old table written to between (gengc.lua, a 5.4.0 bug).
do
  local old = {10}
  collectgarbage()
  local co = coroutine.create(function()
    local x = nil
    local f = function() return x[1] end
    x = coroutine.yield(f)
    coroutine.yield()
  end)
  local _, f = coroutine.resume(co)
  collectgarbage("step", 0)
  old[1] = {"hello"}
  coroutine.resume(co, {123})
  co = nil
  collectgarbage("step", 0)
  collectgarbage("step", 0)
  print("E", f(), old[1][1])
end

-- F. An old all-weak table loses a new object nothing else holds, at the
-- next young collection (gengc.lua).
do
  local t = setmetatable({}, {__mode = "kv"})
  collectgarbage()
  t[1] = {10}
  collectgarbage("step", 0)
  collectgarbage("step", 0)
  t[1] = {10}
  collectgarbage("step", 0)
  print("F", t[1])
end

-- G. Finalizers of objects of every age: a new one and a surviving one
-- dropped are found by a young collection; an old one dropped by a full
-- collection; an old one holding new objects, and a new one holding old.
do
  collectgarbage()
  local new = setmetatable({}, fin("new"))
  new = nil
  collectgarbage("step", 0)
  flush("G new")
  local survivor = setmetatable({}, fin("survival"))
  collectgarbage("step", 0)
  survivor = nil
  collectgarbage("step", 0)
  collectgarbage("step", 0)
  flush("G survival")
  local old = setmetatable({}, fin("old"))
  local oldgraph = {"kept"}
  collectgarbage()
  old.child = {"young child"}
  old = nil
  collectgarbage()
  flush("G old")
  local holder = setmetatable({graph = oldgraph}, {__gc = function(o)
    log[#log + 1] = "holds " .. o.graph[1]
  end})
  holder = nil
  collectgarbage("step", 0)
  collectgarbage("step", 0)
  flush("G young holds old")
end

-- H. Weak tables across ages: an old weak table given new keys and
-- values; ephemerons with an old key and a new value, a new key and a new
-- value, a new key and an old value.
do
  local wk = setmetatable({}, {__mode = "k"})
  local wv = setmetatable({}, {__mode = "v"})
  local e = setmetatable({}, {__mode = "k"})
  local oldkey, oldvalue = {}, {}
  collectgarbage()
  wk[{}] = 1
  wv[1] = {}
  e[oldkey] = {"young value"}
  e[{}] = {}
  e[{}] = oldvalue
  local kept = {}
  wk[kept] = 2
  wv[2] = kept
  collectgarbage("step", 0)
  collectgarbage("step", 0)
  print("H", count(wk), count(wv), count(e), e[oldkey][1], wv[2] == kept)
  collectgarbage()
  print("H full", count(wk), count(wv), count(e))
end

-- I. The mode of an old table's metatable changed: new entries go or stay
-- as the present mode says.
do
  local mt = {}
  local t = setmetatable({}, mt)
  collectgarbage()
  mt.__mode = "v"
  t[1] = {}
  collectgarbage("step", 0)
  collectgarbage("step", 0)
  print("I weak", t[1])
  mt.__mode = nil
  t[2] = {}
  collectgarbage("step", 0)
  collectgarbage("step", 0)
  print("I strong", type(t[2]))
end

-- J. Switching modes with weak tables and finalizers pending, every
-- round: nothing lost, nothing finalized twice.
do
  local keep = {}
  local wv = setmetatable({}, {__mode = "v"})
  local finalized = 0
  local fmt = {__gc = function() finalized = finalized + 1 end}
  for round = 1, 60 do
    keep[round] = {round}
    wv[round] = keep[round]
    wv[-round] = {}
    setmetatable({}, fmt)
    if round % 2 == 0 then
      collectgarbage("incremental")
    else
      collectgarbage("generational")
    end
    if round % 7 == 0 then collectgarbage("step", 0) end
  end
  collectgarbage("generational")
  collectgarbage()
  collectgarbage()
  local sum = 0
  for i = 1, 60 do sum = sum + keep[i][1] end
  print("J", sum, count(wv), finalized)
end

-- K. A structure that grows and stays live (Lua's bad collections fall
-- back on whole cycles), then is dropped and found.
do
  local grow = {}
  for i = 1, 3000 do
    grow[i] = {i, tostring(i)}
    if i % 500 == 0 then collectgarbage("step", 0) end
  end
  local sum = 0
  for i = 1, #grow do sum = sum + grow[i][1] end
  local w = setmetatable({grow[1]}, {__mode = "v"})
  grow = nil
  collectgarbage()
  print("K", sum, w[1])
end

-- M. A step that clears the debt is a young collection, even when memory
-- has grown past the major multiplier: an old object dropped is not found
-- (a 5.4.9 detail the milestone review found).
do
  collectgarbage("generational")
  local ran = 0
  local keep = {}
  local o = setmetatable({}, {__gc = function() ran = ran + 1 end})
  collectgarbage()
  o = nil
  collectgarbage("stop")
  for i = 1, 3000 do keep[i] = {i} end
  print("M", collectgarbage("step", 0), ran, collectgarbage("step", 0), ran)
  collectgarbage("restart")
  collectgarbage()
  print("M full", ran)
end

-- L. Back to incremental.
print("L", collectgarbage("incremental"), collectgarbage("incremental"))
