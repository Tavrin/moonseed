local calls = {}
local n = 0
local it = setmetatable({ name = "it" }, { __call = function(self, s, c, extra)
  n = n + 1
  calls[n] = self.name
  n = n + 1
  calls[n] = s
  if extra ~= nil then n = n + 1 calls[n] = "extra" end
  if c == nil then return 1 end
  if c == 1 then return 2 end
  return nil
end })
local sum = 0
for x in it, "S" do sum = sum + x end
local total = 0
for x in upto, 3 do total = total + x end
local seen = 0
local a1, b1, c1
for a, b, c in many do seen = seen + 1 a1 = a b1 = b c1 = c break end
local nat = setmetatable({}, { __call = second })
local got
for x in nat, 5 do got = x break end
local chain = setmetatable({}, { __call = it })
local via = 0
for x in chain, "T" do via = via + 1 end
local pok, perr
for ok, v in pcall, function(c) error("x", 0) end do pok = ok perr = v break end
return sum, n, calls[1], calls[2], calls[5], total, seen, a1, b1, c1, got, via, calls[7], calls[8] == chain, pok, perr
