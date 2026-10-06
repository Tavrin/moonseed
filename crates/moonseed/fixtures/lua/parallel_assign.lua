-- Lua 5.4 parallel assignment. The index is the old i.
-- i == 2, t[1] == 99, t[2] == nil
-- Aliased targets store right to left, so t[1] ends as 2, not 3.

local i, t = 1, {}
i, t[i] = 2, 99
assert(i == 2 and t[1] == 99 and t[2] == nil)

local u = { 1 }
u[1], u[u[1]] = 2, 3
assert(u[1] == 2)

print(string.format("%d %d %s %d", i, t[1], tostring(t[2]), u[1]))
