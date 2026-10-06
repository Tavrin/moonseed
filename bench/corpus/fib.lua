-- Recursive calls: fib(34) with a global-free local function.
local function fib(n)
  if n < 2 then return n end
  return fib(n - 1) + fib(n - 2)
end
print("fib", fib(34))
