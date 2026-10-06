-- `collectgarbage("step")` in generational mode, state by state, from a
-- small heap so the growth that makes a major collection bad is small
-- (ADR 0051).

-- N. `step` by state (Lua's `genstep` and `stepgenfull`): a young
-- collection; a major one, due from growth with the collector stopped,
-- that keeps everything (bad) and leaves the collector generational, so
-- false; falling back, a whole cycle, true while what it keeps still grows
-- and false once it returns to young collections; a young collection
-- that finalizes a young object before the step returns.
do
  collectgarbage("generational")
  print("N1", collectgarbage("step", 0))
  collectgarbage("stop")
  local keep = {}
  local function fill(from, to)
    for i = from, to do
      local t = {}
      for j = 1, 30 do t[j] = j end
      keep[i] = t
    end
  end
  fill(1, 400)
  print("N2", collectgarbage("step", 100000))
  fill(401, 600)
  print("N3", collectgarbage("step", 0))
  print("N4", collectgarbage("step", 0))
  print("N5", collectgarbage("step", 0))
  local n = 0
  setmetatable({}, {__gc = function() n = n + 1 end})
  print("N6", collectgarbage("step", 0), n)
  collectgarbage("restart")
  print("N7", collectgarbage("step", 1), collectgarbage("isrunning"), #keep)
end

collectgarbage()
print("N8", collectgarbage("step", 0))
