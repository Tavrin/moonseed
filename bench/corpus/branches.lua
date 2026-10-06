local sum = 0
for i = 1, 16000000 do
  if i % 7 == 0 then sum = sum + 3
  elseif i % 3 == 0 then sum = sum - 2
  else sum = sum + 1 end
end
print("branches", sum)
