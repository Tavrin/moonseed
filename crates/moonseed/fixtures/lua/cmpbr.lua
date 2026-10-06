-- Branch-only and value-producing comparisons use the same Lua semantics.
local n = 0
if 1 < 2 then n = n + 1 end
if not (2 <= 1) then n = n + 2 end
if 3 > 2 then n = n + 4 end
if not not (3 >= 3) then n = n + 8 end
if 2 ~= 3 then n = n + 16 end
if 2 == 2 then n = n + 32 end
if not (1.0 < 2.0) then error('float lt') end
if 2.0 <= 1.0 then error('float le') end
if 1.0 == 2.0 then error('float eq') end
if not (1.0 ~= 2.0) then error('float ne') end
local trace = ''
local function left() trace = trace .. 'l' return 1 end
local function right() trace = trace .. 'r' return 2 end
if left() >= right() then error('operand order') end
assert(trace == 'lr')
if (false or 1 < 2) and ('yes' or left() < right()) then
  assert(trace == 'lr')
else error('short circuit') end
if not (2 < 1 or (nil and left() < right())) then
  assert(trace == 'lr')
else error('nested negation') end
if (1 < 2 and false) or (2 < 1 and error('unreachable')) then error('false condition') end
local nan = 0 / 0
if nan == nan then error('nan eq') end
if nan < 0 then error('nan lt') end
if nan <= nan then error('nan le') end
if nan ~= nan then n = n + 64 end
if 9007199254740993 <= 9007199254740992.0 then error('rounded integer') end
if 9007199254740992.0 < 9007199254740993 then n = n + 128 end
if 'a\0b' < 'a\0c' then n = n + 256 end
local a, b = {}, {}
local calls = 0
local mt = {
  __eq = function(x, y) assert(x == a and y == b) calls = calls + 1 return 0 end,
  __lt = function(x, y) assert(x == b and y == a) calls = calls + 1 return false end,
  __le = function(x, y) assert(x == b and y == a) calls = calls + 1 return 'yes' end,
}
setmetatable(a, mt)
setmetatable(b, mt)
if a == b then n = n + 512 end
if a ~= b then error('negation') end
if not (a > b) then n = n + 1024 end
if a >= b then n = n + 2048 end
local value = a == b
local i = 0
while i < 2 do i = i + 1 end
repeat i = i - 1 until i == 0 and not (i < 0)
return n, calls, value, i
