local function tail(n, sum)
  if n == 0 then return sum end
  return tail(n - 1, sum + n)
end
print("tail_recursion", tail(10000000, 0))
