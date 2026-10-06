#!/usr/bin/env python3
"""Generate the string library differential corpora (Phase 3.23):

    tools/gen_string_corpora.py crates/moonseed/fixtures/lua

Deterministic: a fixed seed. The output is compared line by line with Lua
5.4.9 by `lua54_oracle_matches_the_string_fixtures_and_corpora`.
"""
import random, sys

PRELUDE = r'''local function esc(s)
  return (string.gsub(string.format("%q", s), "[\128-\255]", function(c) return "\\" .. c:byte() end))
end
local function show(...)
  local out = {}
  for i = 1, select('#', ...) do
    local v = select(i, ...)
    -- Lua prints a NaN's sign; Moonseed prints every NaN as nan.
    out[#out + 1] = type(v) == "string" and esc(v) or v ~= v and "nan" or tostring(v)
  end
  print(table.concat(out, " "))
end
local function try(f, ...)
  local r = table.pack(pcall(f, ...))
  if not r[1] then
    -- Lua names a function in an argument error by how it was called.
    r[2] = string.gsub(tostring(r[2]), "^[^:]*:%d+: ", "")
    r[2] = string.gsub(r[2], "(bad argument #%d+ to )'[^']*'", "%1?")
  end
  show(table.unpack(r, 1, r.n))
end
'''

def lua_str(b):
    out = []
    for c in b:
        if c in (34, 92):
            out.append('\\' + chr(c))
        elif 32 <= c < 127:
            out.append(chr(c))
        else:
            out.append('\\%03d' % c)
    return '"' + ''.join(out) + '"'

def ints(rng):
    return rng.choice([0, 1, 2, 3, -1, -2, -3, 5, 10, -10, 100, -100, 'math.mininteger', 'math.maxinteger', 7, -7])

def corpus_string(rng):
    lines = [PRELUDE]
    subjects = [b"", b"a", b"hello", b"\0a\0", b"A\x80\xffz", b"Hello World 123", bytes(range(0, 256, 17))]
    for s in subjects:
        for _ in range(60):
            i, j = ints(rng), ints(rng)
            lines.append(f"try(string.sub, {lua_str(s)}, {i}, {j})")
            lines.append(f"try(string.byte, {lua_str(s)}, {i}, {j})")
        lines.append(f"try(string.sub, {lua_str(s)}, {ints(rng)})")
        lines.append(f"try(string.byte, {lua_str(s)})")
        for n in [-1, 0, 1, 2, 5]:
            lines.append(f"try(string.rep, {lua_str(s)}, {n}, {lua_str(rng.choice([b'', b',', b'--', b'\\0']))})")
        for f in ["upper", "lower", "reverse", "len"]:
            lines.append(f"try(string.{f}, {lua_str(s)})")
    lines.append("do local t = {} for b = 0, 255 do t[#t + 1] = string.char(b) end local s = table.concat(t) show(s:upper() == s:upper(), string.byte(s:upper(), 1, -1)) show(string.byte(s:lower(), 1, -1)) end")
    for v in ['0', '255', '256', '-1', '65.0', '"66"', '"x"', '{}', 'nil', '1.5', 'math.maxinteger']:
        lines.append(f"try(string.char, {v})")
        lines.append(f"try(string.char, 65, {v}, 66)")
    for v in ['12', '-12.5', '1e100', 'math.mininteger', '2^53', '0.1', '-0.0', 'true', 'nil', '{}']:
        for f in ["len", "upper", "reverse", "sub", "byte", "rep"]:
            extra = ", 2" if f == "rep" else ""
            lines.append(f"try(string.{f}, {v}{extra})")
    for v in ['"x"', '"1"', '1.5', 'nil', '{}', '"0x10"', '2^63']:
        lines.append(f"try(string.sub, 'abc', {v})")
        lines.append(f"try(string.rep, 'ab', {v})")
    lines.append("try(string.rep, 'x', 3, 7)")
    return lines

PAT_ATOMS = ['a', 'b', '.', '%a', '%d', '%s', '%w', '%p', '%l', '%u', '%x', '%c', '%g', '%A', '%S', '%z', '[ab]', '[^ab]', '[a-c]', '[%d_]', '[]]', '[^]]', '%.', '%%', '\\0', '\\200']
PAT_Q = ['', '', '', '*', '+', '-', '?']

def gen_pattern(rng, depth=0):
    parts = []
    if rng.random() < 0.15:
        parts.append('^')
    for _ in range(rng.randint(1, 4)):
        r = rng.random()
        if r < 0.12 and depth < 2:
            inner = gen_pattern(rng, depth + 1).lstrip('^').rstrip('$')
            parts.append('(' + inner + ')')
        elif r < 0.17:
            parts.append('()')
        elif r < 0.22:
            parts.append(rng.choice(['%b()', '%bab', '%b[]']))
        elif r < 0.27:
            parts.append(rng.choice(['%f[%a]', '%f[%A]', '%f[%z]', '%f[%w_]']))
        elif r < 0.32:
            parts.append('%' + str(rng.randint(1, 3)))
        else:
            parts.append(rng.choice(PAT_ATOMS) + rng.choice(PAT_Q))
    if rng.random() < 0.12:
        parts.append('$')
    if rng.random() < 0.04:
        parts.append(rng.choice(['[a', '%', '(', ')', '%b', '%f', '%9']))
    return ''.join(parts)

SUBJECTS = ["", "a", "ab", "aab", "hello world", "abc123def", "(a(b)c)", "a.b.c", "aaa bbb", "x=1, y=2", "\\0a\\0b", "  pad  ", "ABCabc", "a\\200b", "[x]"]

def corpus_pattern(rng):
    lines = [PRELUDE, 'local function all(it) local out = {} for a, b in it do out[#out + 1] = tostring(a) .. (b ~= nil and "=" .. tostring(b) or "") end return table.concat(out, ",") end',
             'local function gm(s, p, i) return all(string.gmatch(s, p, i)) end',
             'local up = function(...) local t = table.pack(...) for i = 1, t.n do t[i] = tostring(t[i]) end return "<" .. table.concat(t, "|") .. ">" end',
             'local keep = function(c) if c == "a" then return false end return nil end',
             'local tbl = setmetatable({ a = "A", b = 1, ["1"] = "one" }, { __index = function(t, k) if k == "c" then return "C" end end })']
    for _ in range(1400):
        s = rng.choice(SUBJECTS)
        p = gen_pattern(rng)
        ps = '"' + p.replace('"', '\\"') + '"'
        init = rng.choice(['', '', '', ', 1', ', 2', ', -2', ', 0', ', 10'])
        kind = rng.random()
        if kind < 0.25:
            lines.append(f'try(string.find, "{s}", {ps}{init})')
        elif kind < 0.45:
            lines.append(f'try(string.match, "{s}", {ps}{init})')
        elif kind < 0.6:
            lines.append(f'try(gm, "{s}", {ps}{init})')
        else:
            repl = rng.choice(['"%0"', '"<%1>"', '"-"', '""', '"%%"', '"%2%1"', '"%a"', '"x%"', 'up', 'keep', 'tbl', '7'])
            n = rng.choice(['', '', ', 1', ', 0', ', 2', ', -1'])
            lines.append(f'try(string.gsub, "{s}", {ps}, {repl}{n})')
    for s in ["abc", "a.b", "a+b", ""]:
        for p in [".", "+", "", "b", "a.b", "%"]:
            lines.append(f'try(string.find, "{s}", "{p}", 1, true)')
            lines.append(f'try(string.find, "{s}", "{p}", 2, 1)')
    return lines

FLAGS = ['', '-', '+', ' ', '#', '0', '-0', '+ ', '-#', '0#', '+0', '- ']
def corpus_format(rng):
    lines = [PRELUDE]
    ivals = ['0', '1', '-1', '42', '-42', '255', '2^31', '-2^31', 'math.maxinteger', 'math.mininteger', '65', '3.0', '"12"', '1.5', '"x"', 'nil']
    fvals = ['0.0', '-0.0', '1.0', '0.5', '1.5', '2.5', '-2.5', '0.1', '1/3', '1e300', '1e-300', '2^-1074', '2^-1022', '1.7976931348623157e308', '123456789', '1e15', '1e16', '1e100', '1/0', '-1/0', '3', '"2.5"', '-1e-5', '0.0001', '99999.5', '"x"']
    svals = ['"abc"', '""', '"hello world"', '12', '1.5', 'true', 'nil', '"a\\0b"', 'string.rep("x", 120)']
    for _ in range(2500):
        conv = rng.choice('diuoxXcaAeEfgGsq%') if rng.random() < 0.97 else rng.choice('pFnLlhbz')
        flags = rng.choice(FLAGS)
        width = rng.choice(['', '', '1', '5', '10', '20', '99', '100', '05'])
        prec = rng.choice(['', '', '.', '.0', '.1', '.3', '.10', '.20', '.99', '.100'])
        spec = '%' + flags + width + prec + conv
        if conv in 'diuoxXc':
            v = rng.choice(ivals)
        elif conv in 'aAeEfgGF':
            v = rng.choice(fvals)
        elif conv == 'q':
            v = rng.choice(ivals + fvals[:10] + svals)
        else:
            v = rng.choice(svals)
        lines.append(f'try(string.format, "[{spec}]", {v})')
    for v in ['0/0', '-(0/0)']:
        lines.append(f'show((string.format("%f %g %e %a %q", {v}, {v}, {v}, {v}, {v}):gsub("%-nan", "nan"):gsub("%-%(0/0%)", "(0/0)")))')
    lines.append('try(string.format, "%d %s")')
    lines.append('try(string.format, "%")')
    lines.append('try(string.format)')
    lines.append('try(string.format, 12)')
    return lines

def corpus_pack(rng):
    lines = [PRELUDE, 'local function hex(s) return (string.gsub(s, ".", function(c) return string.format("%02x", c:byte()) end)) end',
             'local function pk(fmt, ...) return hex(string.pack(fmt, ...)) end',
             'local function rt(fmt, ...) local p = string.pack(fmt, ...) return string.unpack(fmt, p) end']
    int_opts = ['b', 'B', 'h', 'H', 'i', 'I', 'l', 'L', 'j', 'J', 'T'] + [f'i{n}' for n in (1, 2, 3, 4, 5, 7, 8, 9, 16)] + [f'I{n}' for n in (1, 2, 3, 4, 8, 9, 16)]
    vals = ['0', '1', '-1', '127', '128', '-128', '255', '256', '32767', '-32768', '65535', '2^31', '-2^31', '2^32', 'math.maxinteger', 'math.mininteger', '"7"', '1.0', '1.5', '"x"']
    for _ in range(900):
        endian = rng.choice(['', '', '<', '>', '=', '!', '!2', '!4', '!8', '<!4'])
        o = rng.choice(int_opts)
        v = rng.choice(vals)
        lines.append(f'try(pk, "{endian}{o}", {v})')
        lines.append(f'try(rt, "{endian}{o}", {v})')
    for _ in range(300):
        items = []
        args = []
        for _ in range(rng.randint(1, 5)):
            k = rng.random()
            if k < 0.4:
                items.append(rng.choice(int_opts)); args.append(rng.choice(vals[:12]))
            elif k < 0.55:
                items.append(rng.choice(['f', 'd', 'n'])); args.append(rng.choice(['0.5', '-2.25', '1/0', '1e300', '0.1', '3']))
            elif k < 0.7:
                items.append(rng.choice(['z', 's', 's1', 's2', 'c3', 'c1'])); args.append(rng.choice(['"abc"', '""', '"a\\0b"', '"xyzw"']))
            elif k < 0.8:
                items.append(rng.choice(['x', 'Xi4', 'Xh', ' ', '<', '>', '!4', 'Xb']))
            else:
                items.append(rng.choice(['i17', 'i0', 'q', 'c', 'X', 'Xz', '!3', 's17']))
        fmt = ' '.join(items)
        a = ', '.join(args)
        lines.append(f'try(pk, "{fmt}"{", " + a if a else ""})')
        lines.append(f'try(string.packsize, "{fmt}")')
        lines.append(f'try(rt, "{fmt}"{", " + a if a else ""})')
    for data in ['"\\1\\2\\3\\4\\5\\6\\7\\8"', '"abc\\0def\\0"', '""', '"\\255\\255\\255\\255\\255\\255\\255\\255\\255"']:
        for fmt in ['b', 'i4', 'z', 's1', 'I9', 'i9', 'c2 c2', 'd', 'f', 'Xi4 b', '!4 b i4']:
            for init in ['', ', 2', ', -3', ', 0', ', 20']:
                lines.append(f'try(string.unpack, "{fmt}", {data}{init})')
    return lines

rng = random.Random(3023)
for name, gen in [("corpus_string", corpus_string), ("corpus_pattern", corpus_pattern), ("corpus_format", corpus_format), ("corpus_pack", corpus_pack)]:
    lines = gen(rng)
    with open(f"{sys.argv[1]}/{name}.lua", "w") as f:
        f.write("-- Generated by tools/gen_string_corpora.py (Phase 3.23); compared line by line with Lua 5.4.9.\n")
        head, body = lines[:1], lines[1:]
        # Chunks of 150 lines, each its own function: a prototype holds at
        # most 10,000 instructions in Moonseed.
        out = list(head)
        pre = [l for l in body if l.startswith("local ")]
        rest = [l for l in body if not l.startswith("local ")]
        out += pre
        for i in range(0, len(rest), 150):
            out.append("do (function()")
            out += rest[i:i + 150]
            out.append("end)() end")
        f.write("\n".join(out) + "\n")
