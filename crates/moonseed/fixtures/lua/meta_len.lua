local t = setmetatable({ 1, 2, 3 }, {
    __len = function(self)
        return 99, 100
    end
})
local plain = { 1, 2 }
local empty = setmetatable({}, {
    __len = function()
    end
})

return #t, rawlen(t), #plain, #"abc", #"", rawlen("hello"), #empty
