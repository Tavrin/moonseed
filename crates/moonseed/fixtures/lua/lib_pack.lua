-- string.pack, string.packsize, string.unpack (ADR 0034). Output must match
-- Lua 5.4.9 on x86-64, whose native sizes are Moonseed's canonical ones.
local function hex(s)
  return (string.gsub(s, ".", function(c) return string.format("%02x", string.byte(c)) end))
end
local function show(...)
  local out = {}
  for i = 1, select('#', ...) do
    local v = select(i, ...)
    -- `%q` keeps bytes past 127 as they are; write them as escapes.
    out[#out + 1] = type(v) == "string"
        and (string.gsub(string.format("%q", v), "[\128-\255]", function(c) return "\\" .. c:byte() end))
      or tostring(v)
  end
  print(table.concat(out, " "))
end
local p = string.pack
show(hex(p("i4 >i2 z s1 d", 100, -2, "hello", "xy", 1.5)), string.unpack("i4 >i2 z s1 d", p("i4 >i2 z s1 d", 100, -2, "hello", "xy", 1.5)))
show(hex(p("b B h H l L j J T", -1, 255, -2, 65535, -3, 3, -4, 4, 5)))
show(hex(p("i1 i2 i3 i5 i7 i8 i9 i16", 1, 2, 3, 4, 5, 6, -7, -8)))
show(hex(p("I1 I3 I9 I16", 255, 0xffffff, 1, 2)))
show(hex(p("<i4 >i4 =i4 !4 i1 i4 !8 i1 d", 1, 1, 1, 1, 2, 3, 4.0)))
show(hex(p("f d n", 1.5, -2.25, 1/0)), hex(p("f", 0.1)), hex(p("c5", "ab")), hex(p("s2 s", "q", "r")))
show(hex(p("x i2 Xi4 i4 x", 1, 2)), hex(p(" < i2  > i2 ", 1, 1)), hex(p("")))
show(string.packsize("i4 i8 !4 i2 d"), string.packsize("b h i l j T f d n c10"), string.packsize("!8 b Xd"), string.packsize(""))
show(pcall(string.packsize, "s"))
show(pcall(string.packsize, "z"))
show(pcall(string.pack, "i17", 1))
show(pcall(string.pack, "i0", 1))
show(pcall(string.pack, "i1", 200))
show(pcall(string.pack, "I1", -1))
show(pcall(string.pack, "q", 1))
show(pcall(string.pack, "c", "x"))
show(pcall(string.pack, "c2", "xyz"))
show(pcall(string.pack, "z", "a\0b"))
show(pcall(string.pack, "s1", string.rep("x", 300)))
show(pcall(string.pack, "Xz", 1))
show(pcall(string.pack, "!3 i4", 1))
show(pcall(string.pack, "i4", "x"))
show(pcall(string.pack, "i4"))
show(pcall(string.pack, "d", {}))
show(string.unpack("B", "\255"), string.unpack("<h", "\1\2"), string.unpack(">h", "\1\2"), string.unpack("b", "\128"))
show(string.unpack("i16", string.rep("\255", 16)), pcall(string.unpack, "i16", "\0\0\0\0\0\0\0\0\1" .. string.rep("\0", 7)))
show(string.unpack("I9", "\1\0\0\0\0\0\0\0\0"), pcall(string.unpack, "I9", string.rep("\0", 8) .. "\1"))
show(pcall(string.unpack, "i4", "ab"))
show(pcall(string.unpack, "b", "x", 5))
show(string.unpack("b", "xy", -1), string.unpack("b", "xy", 2), pcall(string.unpack, "b", "xy", 3))
show(string.unpack("z z", "ab\0cd\0"), pcall(string.unpack, "z", "abc"))
show(string.unpack("s1", "\3abc"), pcall(string.unpack, "s1", "\9abc"))
show(string.unpack("c3", "abcdef", 2), string.unpack("i2 i2", p("i2 i2", 7, -7)))
show(string.unpack("f d", p("f d", 0.1, 0.1)))
show(select("#", string.unpack(string.rep("b", 200), string.rep("\1", 200))))
show(string.unpack("!4 b i4", p("!4 b i4", 1, 2)))
show(string.unpack("=I2", "\1\0"), string.unpack("j", p("j", math.mininteger)), string.unpack("J", p("J", -1)))
return "done"
