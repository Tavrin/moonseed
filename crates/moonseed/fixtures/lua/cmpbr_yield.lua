-- Every comparison metamethod yields; 0 is true, false/nil are false.
local calls = 0
local co = coroutine.create(function()
  local mt = {
    __eq = function() calls = calls + 1 coroutine.yield('eq') return false end,
    __lt = function() calls = calls + 1 coroutine.yield('lt') return 0 end,
    __le = function() calls = calls + 1 coroutine.yield('le') return nil end,
  }
  local a, b = setmetatable({}, mt), setmetatable({}, mt)
  local answer = 0
  if a == b then error('eq') end
  if a ~= b then answer = answer + 1 end
  if (a == b) or (a < b and 'yes') then answer = answer + 2 end
  if not (a > b) then error('gt') end
  if a <= b then error('le') end
  if not (a >= b) then answer = answer + 4 end
  return answer
end)
local resumes, result = 0
while coroutine.status(co) ~= 'dead' do
  local ok, value = coroutine.resume(co)
  assert(ok)
  resumes = resumes + 1
  result = value
end
assert(calls == 7 and resumes == 8 and result == 7)
return result, calls, resumes
