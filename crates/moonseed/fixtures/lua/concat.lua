local t = setmetatable({}, {})
local label = { x = "x", a = "a", b = "b" }
label[t] = "T"
label[1] = "1"
getmetatable(t).__concat = function(a, b)
    return label[a] .. "+" .. label[b]
end
local native = setmetatable({}, { __concat = second })
local empty = setmetatable({}, { __concat = function() end })

return 1 .. 2, 1.5 .. 2, -0.0 .. "", 2 ^ 63 .. "", 2 ^ 53 .. "", 1e100 .. "", 9223372036854775807 .. "",
    (-9223372036854775807 - 1) .. "", 1 / 0 .. "", -1 / 0 .. "", 3.0 .. "", 0.1 .. "", 1e15 .. "|", "a" .. "b" .. "c",
    t .. "x", "x" .. t, "a" .. t .. "b", 1 .. t, native .. 5, empty .. "x" == nil, "n" .. 10 // 3
