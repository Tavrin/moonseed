local v = coroutine.yield("fixture-yield", nil, 9)
return v, 10, nil
