-- string.format (ADR 0034). Output must match Lua 5.4.9, `%p` aside.
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
local f = string.format
show(f("%d %i %u %o %x %X %c", 42, -7, 3, 8, 255, 255, 65), f("%5d|%-5d|%05d|%+d|% d", 1, 2, 3, 4, 5))
show(f("%.3d|%.0d|%#o|%#x|%#X", 7, 0, 8, 255, 255), f("%d %d", math.maxinteger, math.mininteger), f("%u", -1))
show(f("%f %e %g %E %G", 3.14159, 31415.9, 0.0001, 1e20, 1e-20), f("%.0f %.0f %.0f %.0f", 0.5, 1.5, 2.5, -0.5))
show(f("%10.3f|%-10.2e|%+.1g|% g|%#g|%#.0f", 3.14159, 2.5, 0.05, 7.0, 1.0, 2.0))
show(f("%.20f", 0.1), f("%.17g", 0.1), f("%g %g %g", 1e15, 1e16, 123456789012), f("%.99f", 1.0):len())
show(f("%a %A %.3a %.0a", 1.0, 0.5, 1 / 3, 1.5), f("%a %a", 2^-1074, -0.0), f("%5.1f|", -0.0))
show(f("%f %e %g %a", 1/0, -1/0, 1/0, -1/0), f("%5.1f|%-6g|", 1/0, -1/0))
show(f("%s %s %s %s", nil, true, 12.5, 7), f("%10s|%-10s|%.2s|%5.1s|", "abc", "abc", "abc", "abc"))
show(f("%s", "a\0b"), pcall(f, "%10s", "a\0b"))
show(f("%s", string.rep("x", 120)):len(), f("%.3s", string.rep("y", 120)), f("%-3s|", string.rep("z", 100)):len())
local t = setmetatable({}, { __tostring = function() return "TT" end })
local n = setmetatable({}, { __tostring = function() return 42 end })
show(f("[%s] [%5s] [%-5s|]", t, t, t), f("%s", n))
show(pcall(f, "%s", setmetatable({}, { __tostring = function() return {} end })))
show(f("%q", "a\nb\0c\"d\\e\r\1\0012\200"), f("%q", 1/3), f("%q", math.mininteger), f("%q", 255))
show(f("%q %q %q %q", 1/0, -1/0, 2^53, -0.0), f("%q %q %q", nil, true, false))
show(pcall(f, "%q", {}))
show(pcall(f, "%10q", "x"))
show(pcall(f, "%d", 3.5))
show(pcall(f, "%d", "3"), pcall(f, "%d", "x"))
show(pcall(f, "%d"))
show(pcall(f, "%y", 1))
show(pcall(f, "%100d", 1))
show(pcall(f, "%.100f", 1))
show(pcall(f, "%#d", 1))
show(pcall(f, "%-+ #0.3c", 1))
show(pcall(f, "%", 1))
show(pcall(f, "%l", 1))
show(pcall(f, "%1234567890123456789012345678901234567890d", 1))
show(f("%5c|%-5c|", 65, 66), f("%%|%%%%"), f("no items"), f("%s%s", "a", "b"))
show(f("%.14g", 2^63), f("%.3f", 2.0005), f("%.2f", 2.675), f("%e", 0), f("%g", 100000), f("%g", 1e-5))
show(f("%5.2s|", "abcdef"), f("%.0s|", "abc"), f("%x", -1), f("%o", math.mininteger))
return "done"
