#!/usr/bin/env python3
"""Deterministic extended-UTF-8 cases and one shared Lua driver protocol."""
from __future__ import annotations

import argparse
import collections
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CORPUS = ROOT / 'tests' / 'utf8'
BATCH_SIZE = 256
SEED = 0x33605409
BOUNDARIES = (0, 0x7f, 0x80, 0x7ff, 0x800, 0xd7ff, 0xd800, 0xdfff,
              0xe000, 0xffff, 0x10000, 0x10ffff, 0x110000, 0x1fffff,
              0x200000, 0x3ffffff, 0x4000000, 0x7fffffff)


def quote(value: str | bytes) -> str:
    data = value.encode('ascii') if isinstance(value, str) else value
    return '"' + ''.join(f'\\{b:03d}' for b in data) + '"'


def encode(value: int, length: int | None = None) -> bytes:
    """Test-data encoder, including deliberately overlong/out-of-range forms."""
    if length is None:
        length = next(n for n, limit in enumerate(
            (0x80, 0x800, 0x10000, 0x200000, 0x4000000, 0x80000000), 1)
                      if value < limit)
    if length == 1:
        return bytes([value])
    tail = []
    for _ in range(length - 1):
        tail.append(0x80 | (value & 63))
        value >>= 6
    return bytes([(0xff << (8 - length)) & 255 | value] + tail[::-1])


def case(group: str, name: str, source: str) -> dict:
    return dict(group=group, id=f'{group}.{name}', chunk='@utf8-case.lua', source=source)


def decoder(group: str, name: str, subject: bytes):
    s = quote(subject)
    for mode, source in (
        ('len.strict', f'return utf8.len({s},1,-1,false)'),
        ('len.lax', f'return utf8.len({s},1,-1,true)'),
        ('codepoint.strict', f'return utf8.codepoint({s},1,#{s},false)'),
        ('codepoint.lax', f'return utf8.codepoint({s},1,#{s},true)'),
        ('codes.strict', collect_codes(s, 'false')),
        ('codes.lax', collect_codes(s, 'true')),
        ('offset', f'return utf8.offset({s},1)')):
        yield case(group, f'{name}.{mode}', source)


def collect_codes(subject: str, lax: str) -> str:
    # The supplied partial-result window survives a caught iterator error.
    return (f'local out=...; for p,v in utf8.codes({subject},{lax}) do '
            'out.n=out.n+1; out[out.n]=p; out.n=out.n+1; out[out.n]=v end; '
            'return table.unpack(out,1,out.n)')


def boundary_subjects():
    out = collections.OrderedDict()
    values = sorted({v + d for v in BOUNDARIES for d in (-1, 0, 1)
                     if 0 <= v + d <= 0x7fffffff})
    for v in values:
        data = encode(v)
        out[f'cp{v:x}'] = data
        for n in range(len(data)):
            out[f'cp{v:x}.truncated{n}'] = data[:n]
        # Also test an invalid sequence after a valid decoded prefix.
        out[f'cp{v:x}.prefixed'] = b'A\0' + data
        out[f'cp{v:x}.extra_cont'] = data + b'\x80'
        for i in range(1, len(data)):
            out[f'cp{v:x}.bad_cont{i}'] = data[:i] + b'A' + data[i+1:]
    for n in range(2, 7):
        minimum = (0, 0, 0x80, 0x800, 0x10000, 0x200000, 0x4000000)[n]
        for v in (0, 1, minimum - 1):
            out[f'overlong{n}.{v:x}'] = encode(v, n)
    for v in (0x80000000, 0xffffffff):
        out[f'above_max.{v:x}'] = encode(v, 6)
    for lead in (0x80, 0xbf, 0xc0, 0xc1, 0xfe, 0xff):
        out[f'lead{lead:x}.tail'] = bytes([lead]) + b'\x80' * 5
    return out


def subjects():
    return dict(empty=b'', ascii=b'ab', two=encode(0x80), three=encode(0x800),
                four=encode(0x10000), extended=encode(0x7fffffff),
                surrogate=encode(0xd800), malformed=b'A\xc2B',
                continuation=b'\x80\xbf', nul=b'A\0' + encode(0x800),
                mixed=b'A' + encode(0x80) + encode(0x800) + encode(0x10000))


def positions(size):
    return [('zero', '0'), ('one', '1'), ('len', str(size)),
            ('len1', str(size+1)), ('len2', str(size+2)), ('minus1', '-1'),
            ('minuslen', str(-size)), ('before', str(-size-1)),
            ('min', 'math.mininteger'), ('max', 'math.maxinteger'),
            ('integral', '1.0'), ('fractional', '1.5'),
            ('numeric', '"1"'), ('badstr', '"x"'), ('table', '{}'),
            ('false', 'false'), ('nil', 'nil')]


def all_cases():
    for length in (1, 2):
        for v in range(256 ** length):
            yield from decoder(f'decoder{length}', f'{v:0{length*2}x}', v.to_bytes(length, 'big'))
    for name, data in boundary_subjects().items():
        yield from decoder('boundaries', name, data)
    char_values = [(f'cp{v:x}', str(v)) for v in BOUNDARIES]
    char_values += [('negative', '-1'), ('above', '0x80000000'),
                    ('maxinteger', 'math.maxinteger'), ('mininteger', 'math.mininteger'),
                    ('float_ok', '65.0'), ('float_bad', '65.5'), ('numeric', '"65"'),
                    ('badstr', '"bad"'), ('boolean', 'false'), ('nil', 'nil'),
                    ('table', '{}'), ('function', 'function() end'),
                    ('thread', 'coroutine.create(function() end)'), ('nan', '0/0'),
                    ('inf', 'math.huge')]
    for name, value in char_values:
        yield case('char', name, f'return utf8.char({value})')
    yield case('char', 'zero_args', 'return utf8.char()')
    yield case('char', 'many_args', 'local t={}; for i=1,1000 do t[i]=(i*7919)%0x80000000 end; return utf8.char(table.unpack(t))')
    yield case('char', 'bad_late_arg', 'return utf8.char(65,66,-1)')
    lexer_values = [(f'cp{v:x}', str(v), v) for v in BOUNDARIES]
    lexer_values += [('float_ok', '65.0', 65), ('numeric', '"65"', 65)]
    lexer_values += [(f'many{i:04d}', str((i*7919)%0x80000000), (i*7919)%0x80000000)
                     for i in range(1,1001)]
    for name, value, v in lexer_values:
        inner = 'return "\\u{' + f'{v:x}' + '}"'
        yield case('lexer', name,
                   f'local f,e=load({quote(inner)},"@utf8-escape.lua"); if not f then error(e,0) end; '
                   f'local a,b=utf8.char({value}),f(); return a,b,a==b')
    for name, data in subjects().items():
        s = quote(data)
        pos = positions(len(data))
        for fn in ('len', 'codepoint', 'codes'):
            yield case('defaults', f'{name}.{fn}', f'return utf8.{fn}({s})')
        for iname, i in pos:
            for fn in ('len', 'codepoint'):
                yield case('defaults', f'{name}.{fn}.{iname}', f'return utf8.{fn}({s},{i})')
        for fn in ('len', 'codepoint'):
            for iname, i in pos:
                for jname, j in pos:
                    for lname, lax in (('strict', 'false'), ('lax', 'true')):
                        yield case('positions', f'{name}.{fn}.{iname}.{jname}.{lname}',
                                   f'return utf8.{fn}({s},{i},{j},{lax})')
        ns = [('zero', '0'), ('one', '1'), ('minus1', '-1'), ('two', '2'), ('minus2', '-2'),
              ('len', str(len(data))), ('minuslen', str(-len(data))),
              ('len1', str(len(data)+1)), ('minuslen1', str(-len(data)-1)),
              ('min', 'math.mininteger'), ('max', 'math.maxinteger')]
        for nname, n in ns:
            yield case('offset_positions', f'{name}.{nname}.default', f'return utf8.offset({s},{n})')
            for iname, i in pos:
                yield case('offset_positions', f'{name}.{nname}.{iname}', f'return utf8.offset({s},{n},{i})')
        for lname, lax in (('false', 'false'), ('nil', 'nil'), ('true', 'true'),
                          ('zero', '0'), ('empty', '""'), ('table', '{}')):
            for fn in ('len', 'codepoint', 'codes'):
                source = collect_codes(s, lax) if fn == 'codes' else f'return utf8.{fn}({s},1,-1,{lax})'
                yield case('lax', f'{name}.{fn}.{lname}', source)
        for lname, lax in (('strict', 'false'), ('lax', 'true')):
            yield case('codes', f'{name}.{lname}.triple', f'return utf8.codes({s},{lax})')
            yield case('codes', f'{name}.{lname}.generic', collect_codes(s, lax))
            controls = pos + [('pastend', str(len(data)+10)), ('nonnumeric', '"no"')]
            for cname, control in controls:
                yield case('codes', f'{name}.{lname}.manual.{cname}',
                           f'local f= utf8.codes({s},{lax}); return f({s},{control})')
            for control in range(2, len(data)+1):
                yield case('codes', f'{name}.{lname}.byte{control}',
                           f'local f= utf8.codes({s},{lax}); return f({s},{control})')
    yield case('codes', 'identity', 'local a,b,c=utf8.codes(""); local d=utf8.codes("A"); local e=utf8.codes("",true); local f=utf8.codes("A",0); return rawequal(a,d),rawequal(e,f),rawequal(a,e),select("#",utf8.codes("")),b,c')
    yield case('charpattern', 'bytes', 'return utf8.charpattern,#utf8.charpattern')
    for name, data in subjects().items():
        yield case('charpattern', name, f'local t={{}}; for s in string.gmatch({quote(data)},utf8.charpattern) do t[#t+1]=s end; return table.unpack(t)')
    bad = [('nil', 'nil'), ('false', 'false'), ('table', '{}'), ('function', 'function() end'),
           ('thread', 'coroutine.create(function() end)'), ('badstr', '"bad"'),
           ('fractional', '1.5'), ('numeric', '"65"'), ('number', '123'),
           ('tostring', 'setmetatable({},{__tostring=function() return "A" end})')]
    for fn, args, arity in (('char', ['65'], 1), ('len', ['"A"','1','1'], 3),
                           ('codepoint', ['"A"','1','1'], 3), ('offset', ['"A"','1','1'], 3),
                           ('codes', ['"A"'], 1)):
        yield case('arguments', f'{fn}.missing', f'return utf8.{fn}()')
        for slot in range(arity):
            for name, value in bad:
                changed = args.copy(); changed[slot] = value
                yield case('arguments', f'{fn}.arg{slot+1}.{name}', 'return utf8.' + fn + '(' + ','.join(changed) + ')')
        if fn != 'char':
            tail = ',1' if fn == 'offset' else ''
            yield case('arguments', f'{fn}.numeric_subject', f'return utf8.{fn}(123{tail})')
        yield case('arguments', f'{fn}.local', f'local f=utf8.{fn}\nreturn f({{}})')
        yield case('arguments', f'{fn}.method', f'local t={{f=utf8.{fn}}}\nt:f()')
        yield case('arguments', f'{fn}.multiline', f'local x=1\n\nreturn utf8.{fn}({{}})')
    for lname, lax in (('strict', 'false'), ('lax', 'true')):
        for name, value in bad:
            yield case('arguments', f'iterator.{lname}.state.{name}', f'local f=utf8.codes("",{lax}); return f({value},0)')
        yield case('arguments', f'iterator.{lname}.missing', f'local f=utf8.codes("",{lax}); return f()')
    # Xorshift32 fixes the byte stream independently of Python RNG versions.
    state = SEED
    for index in range(4096):
        length = index % 17
        data = bytearray()
        for _ in range(length):
            state ^= (state << 13) & 0xffffffff
            state ^= state >> 17
            state ^= (state << 5) & 0xffffffff
            state &= 0xffffffff
            data.append(state & 255)
        yield from decoder('random', f'{index:04d}', bytes(data))
    yield case('module', 'require', 'local m=require("utf8"); return rawequal(m,utf8),rawequal(m,package.loaded.utf8),rawequal(m,require("utf8"))')
    yield case('module', 'fields', 'local keys={}; for k in pairs(utf8) do keys[#keys+1]=k end; table.sort(keys); return table.unpack(keys)')


DRIVER = r'''local function hex(s)
  local t={}; for i=1,#s do t[i]=string.format('%02x',string.byte(s,i)) end
  return table.concat(t)
end
local function value(v)
  local t=type(v)
  if t=='string' then return t,hex(v) end
  if t=='number' then
    if math.type(v)=='integer' then return 'number.integer',tostring(v) end
    return 'number.float',string.format('%a',v)
  end
  if t=='nil' then return t,'-' end
  if t=='boolean' then return t,tostring(v) end
  if t=='function' then
    local strict=utf8.codes(''); local lax=utf8.codes('',true)
    if rawequal(v,strict) then return t,'strict-iterator' end
    if rawequal(v,lax) then return t,'lax-iterator' end
  end
  error('unsupported protocol value: '..t)
end
local function execute(c)
  local partial={n=0}
  local f,e=load(c[2],'@utf8-case.lua')
  local status,r,et,ev='compile',{n=0},'string',e
  if f then
    r=table.pack(pcall(f,partial))
    if r[1] then status='ok'; et=nil; ev=nil
    else status='error'; et=type(r[2]); ev=r[2]; r=partial end
  end
  local n= status=='ok' and r.n-1 or r.n
  local fields={c[1],status,tostring(n)}
  for i=1,n do
    local t,b=value(r[status=='ok' and i+1 or i]); fields[#fields+1]=t; fields[#fields+1]=b
  end
  if et then local t,b=value(ev); fields[#fields+1]=t; fields[#fields+1]=b
  else fields[#fields+1]='-'; fields[#fields+1]='-' end
  print(table.concat(fields,'\t'))
end
local cases={%s}
for _,c in ipairs(cases) do execute(c) end
'''


def driver(cases):
    rows = ['{' + quote(c['id']) + ',' + quote(c['source']) + '}' for c in cases]
    # Replace only the table placeholder: preserve Lua's %02x and %a formats.
    return DRIVER.replace('{%s}', '{' + ','.join(rows) + '}').encode('ascii')


def grouped_batches(size=BATCH_SIZE):
    batch = []
    group = None
    numbers = collections.Counter()
    seen = set()
    for c in all_cases():
        if c['id'] in seen:
            raise ValueError(f'duplicate case id: {c["id"]}')
        seen.add(c['id'])
        if c['group'] != group or len(batch) == size:
            if batch:
                yield f'{group}-{numbers[group]:04d}', batch
                numbers[group] += 1
            if c['group'] != group:
                group = c['group']
            batch = []
        batch.append(c)
    if batch:
        yield f'{group}-{numbers[group]:04d}', batch


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    counts = collections.Counter()
    with (args.out / 'manifest.jsonl').open('w') as manifest:
        for name, cases in grouped_batches():
            batch = args.out / name; batch.mkdir(parents=True, exist_ok=True)
            (batch / 'driver.lua').write_bytes(driver(cases))
            for c in cases:
                manifest.write(json.dumps(c, sort_keys=True) + '\n')
                counts[c['group']] += 1
    (args.out / 'groups.json').write_text(json.dumps(counts, sort_keys=True, indent=2) + '\n')


if __name__ == '__main__':
    main()
