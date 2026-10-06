-- math.random from explicit seeds (ADR 0032): 1,750 outputs per seed
-- across every argument form. Compared with Lua 5.4.9.
local seeds = { {0, 0}, {1, 0}, {123, 456}, {-1, -1}, {math.mininteger, math.maxinteger}, {42} }
for _, s in ipairs(seeds) do
  print(math.randomseed(s[1], s[2]))
  local acc = {}
  for i = 1, 250 do
    acc[#acc + 1] = math.random(0)
    acc[#acc + 1] = math.random(i)
    acc[#acc + 1] = math.random(-i, i * 3 + 7)
    acc[#acc + 1] = math.random(math.mininteger, math.maxinteger)
    acc[#acc + 1] = math.random(1, 3)
    acc[#acc + 1] = math.random(0, (1 << 40) + 12345)
    acc[#acc + 1] = math.random() * 2^53
  end
  for k = 1, #acc, 7 do print(acc[k], acc[k + 1], acc[k + 2], acc[k + 3], acc[k + 4], acc[k + 5], acc[k + 6]) end
end
return "done"
