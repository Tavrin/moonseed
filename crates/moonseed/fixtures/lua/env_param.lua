local get = function(_ENV)
    return value
end

return get({ value = 5 }), get({ value = 6 })
