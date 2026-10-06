-- `collectgarbage` over Moonseed's collector (ADR 0031).
print(collectgarbage())
print(select("#", collectgarbage("collect")), collectgarbage("collect"))
print(type(collectgarbage("count")), collectgarbage("count") > 0)
print(collectgarbage("isrunning"))
print(collectgarbage("stop"), collectgarbage("isrunning"))
local keep = {}
for i = 1, 200 do keep[i] = { i } end
print(collectgarbage("isrunning"), #keep)
print(collectgarbage("restart"), collectgarbage("isrunning"))
print((pcall(collectgarbage, "bogus")))
print((pcall(collectgarbage, 1)))
print(pcall(collectgarbage, "collect", "x"))
print((pcall(collectgarbage, "step", "x")))
print((pcall(collectgarbage, "step", 1.5)))
print(pcall(collectgarbage, nil))
print(collectgarbage("count", "ignored") > 0)
return "done"
