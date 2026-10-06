local f = add
local t = { f = add }
local keyed = { [add] = 1 }

local g = function()
    return f(1, 1)
end

return add(40, 2), f(40, 2), f == add, t.f(20, 22), add == sub, add ~= sub,
    keyed[add], keyed[sub], g()
