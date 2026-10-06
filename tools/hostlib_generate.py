#!/usr/bin/env python3
"""Deterministic host-library cases; PUC alone supplies expected semantics."""
from __future__ import annotations

import argparse
import collections
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CORPUS = ROOT / 'tests' / 'hostlib'
BATCH_SIZE = 32


def quote(value):
    data = value.encode('utf-8') if isinstance(value, str) else value
    return '"' + ''.join(f'\\{b:03d}' for b in data) + '"'


GROUPS = {
    'io_modes': ('exact', 'UTC', 'empty'),
    'io_read': ('exact', 'UTC', 'empty'),
    'io_numerals': ('exact', 'UTC', 'empty'),
    'io_write': ('exact', 'UTC', 'empty'),
    'io_buffering': ('exact-linux-libc', 'UTC', 'empty'),
    'io_seek': ('exact', 'UTC', 'empty'),
    'io_defaults': ('exact', 'UTC', 'empty'),
    'io_lines': ('exact', 'UTC', 'empty'),
    'io_lifecycle': ('exact+shape-only', 'UTC', 'empty'),
    'io_streams': ('exact+shape-only', 'UTC', 'empty'),
    'io_stdin': ('exact', 'UTC', 'read.txt'),
    'loadfile': ('exact', 'UTC', 'empty'),
    'load_binary': ('shape-only', 'UTC', 'empty'),
    'loadfile_stdin': ('exact', 'UTC', 'chunk.lua'),
    'dofile_stdin': ('exact', 'UTC', 'chunk.lua'),
    'searchpath': ('exact', 'UTC', 'empty'),
    'require': ('exact-filtered-searchers', 'UTC', 'empty'),
    'os_fs': ('platform-specific-linux', 'UTC', 'empty'),
    'os_utc': ('platform-specific-linux', 'UTC', 'empty'),
    'os_new_york': ('platform-specific-linux', 'America/New_York', 'empty'),
    'os_misc': ('exact+shape-only+platform-specific-linux', 'UTC', 'empty'),
    'process': ('requires-process', 'UTC', 'empty'),
    'arg': ('requires-argv', 'UTC', 'empty'),
}


def case(group, name, source):
    return dict(group=group, id=f'{group}.{name}', chunk='@hostlib-case.lua',
                source='local H=...; ' + source)


def all_cases():
    def c(group, name, source):
        return case(group, str(name), source)

    # PUC accepts [rwa]+?b*: '+' precedes b; repeated b is accepted.
    valid_modes = [m+plus+'b'*n for m in ('r', 'w', 'a')
                   for plus in ('', '+') for n in (0, 1, 2, 8)]
    modes = valid_modes + ['rb+', 'wb+', 'ab+', '', 'x', 'rw', 'r++', 'br',
                           'r+b+', 'rt', 'w+t', 'a ', 'R', '+', 'rbx', 'r\0w']
    for i, mode in enumerate(modes):
        for existing in (False, True):
            setup = 'H.put("work.txt","seed")' if existing else 'H.remove("work.txt")'
            yield c('io_modes', f'{i}.{int(existing)}',
                    f'{setup}; local f,e,n=io.open("work.txt",{quote(mode)}); '
                    'if not f then return f,e,n end; local t=io.type(f); '
                    'local ok= f:close(); return t,ok,H.get("work.txt")')
    for i, path in enumerate(('missing.txt', 'absent/child', 'fixtures/empty', 'fixtures/data.txt')):
        yield c('io_modes', f'failure.{i}', f'local f,e,n=io.open({quote(path)},"r"); if f then f:close(); return "opened" end; return f,e,n')
    for i, args in enumerate(('', 'nil', '{}', 'true', '"fixtures/data.txt",{}', '"fixtures/data.txt",true')):
        yield c('io_modes', f'arg.{i}', f'return io.open({args})')
    for i, mode in enumerate(valid_modes):
        read = 'f:read("a")' if mode[0] == 'r' or '+' in mode else '"no-read"'
        write = 'table.pack(f:write("X"))' if mode[0] != 'r' or '+' in mode else 'table.pack("no-write")'
        yield c('io_modes', f'access.{i}', f'H.put("work.txt","seed"); local f=assert(io.open("work.txt",{quote(mode)})); '
                f'local before={read}; local r={write}; local identity=r[1]==f; '
                'f:flush(); f:close(); return before,identity,H.get("work.txt")')
    yield c('io_modes', 'default', 'local f=assert(io.open("fixtures/data.txt")); local s=f:read("a"); f:close(); return s')
    yield c('io_modes', 'nilmode', 'local f=assert(io.open("fixtures/data.txt",nil)); local s=f:read("a"); f:close(); return s')

    subjects = [b'', b'one\ntwo\nlast', b'\n\n', b'A\0B\r\nC', b'12 34\n56\n', b'bad\n7', b'99']
    formats = ['', '"l"', '"L"', '"a"', '"n"', '"*l"', '"*L"', '"*a"', '"*n"',
               '0', '1', '3', '100', '"l","l","l","l"', '"n","n","l"',
               '"l","n","a"', '"n","a","l"', '"a",0,"l"', '0,0,1,0',
               '"bad"', '""', '"*"', '"**l"', '-1', '1.5', '"2"', 'nil', '{}', 'true',
               '"n",{}', '"l",{}', '"n","bad"', '"n",-1', '"l","a",0']
    for si, subject in enumerate(subjects):
        for fi, fmt in enumerate(formats):
            yield c('io_read', f'{si}.{fi}', f'H.put("work.txt",{quote(subject)}); local f <close> =assert(io.open("work.txt","rb")); '
                    f'local r=table.pack(f:read({fmt})); local pos=f:seek(); '
                    'local tail=f:read("a"); H.append(r,pos,tail); return table.unpack(r,1,r.n)')
    for fmt in ('"l"', '"L"', '"a"', '"n"', '0', '1'):
        yield c('io_read', 'eof.' + str(formats.index(fmt)), 'local f <close> =assert(io.open("fixtures/empty","r")); '
                f'local a=table.pack(f:read({fmt})); H.extend(a,table.pack(f:read({fmt}))); return table.unpack(a,1,a.n)')

    numerals = ['0', '-0', '+12', '  \t\r\n42', '0xff', '-0XAB', '0x1.8p+2', '0x.8p0',
                '1e3', '-2.5E-2', '.25', '1.', '1e', '1e+', '0x', '0xp1', '0x1p',
                '+', '-', '.', 'nan', 'inf', '0xg', '123abc', '1.2.3', '1e9999',
                '-1e9999', '1e-9999', '9223372036854775807', '9223372036854775808',
                '-9223372036854775808', '-9223372036854775809', '0xffffffffffffffff',
                '9'*199, '9'*200, '9'*201, '0'*201, '1'+'0'*300]
    for i, numeral in enumerate(numerals):
        yield c('io_numerals', i, f'H.put("work.txt",{quote(numeral+" Z")}); local f <close> =assert(io.open("work.txt","r")); '
                'local r=table.pack(f:read("n","n","a")); H.append(r,f:seek(),f:read("a")); return table.unpack(r,1,r.n)')

    for i, args in enumerate(('', '"abc"', '12', '1.0', '-0.0', '1.25', 'math.huge', '0/0',
                              '"a",12,1.0,"b"', '"A\\0B"', 'nil', 'true', '{}',
                              '"prefix",{}', '"prefix",nil', 'setmetatable({},{__tostring=function() return "x" end})')):
        yield c('io_write', i, 'H.put("work.txt",""); local f <close> =assert(io.open("work.txt","w+")); '
                f'local ok,e=pcall(function() return f:write({args}) end); '
                'local identity=ok and e==f; f:flush(); f:seek("set"); local bytes=f:read("a"); '
                'if ok then return ok,identity,bytes else return ok,e,bytes end')
    yield c('io_write', 'chaining', 'local f <close> =assert(io.open("work.txt","w+")); local same=f:write("a"):write(2):write("b")==f; f:flush(); f:seek("set"); return same,f:read("a")')
    yield c('io_write', 'readonly', 'local f <close> =assert(io.open("fixtures/data.txt","r")); return f:write("x")')
    yield c('io_write', 'flush', 'local f <close> =assert(io.open("work.txt","w")); f:write("hello"); return f:flush(),H.get("work.txt")')
    for mode in ('no', 'full', 'line', 'invalid', ''):
        for size in ('nil', '0', '16', '-1', '1.5'):
            yield c('io_write', f'buffer.{mode or "empty"}.{size}', 'local f <close> =assert(io.open("work.txt","w")); '
                    f'return f:setvbuf({quote(mode)},{size})')

    # PUC passes NULL to libc setvbuf: this Linux oracle uses 4096 bytes,
    # independently of requested size (including the omitted LUAL_BUFFERSIZE=1024).
    for mode in ('no', 'full', 'line'):
        for size in ('nil', '0', '1', '16', '4096', '8192', '16384', '-1'):
            for n in (0, 1, 15, 16, 4095, 4096, 4097, 8191, 8192, 8193):
                yield c('io_buffering', f'bulk.{mode}.{size}.{n}',
                        'local f=assert(io.open("work.txt","w+")); '
                        f'assert(f:setvbuf({quote(mode)},{size})); '
                        f'f:write(string.rep("x",{n})); local a=#H.get("work.txt"); '
                        'f:write("y"); local b=#H.get("work.txt"); '
                        'f:flush(); local d=#H.get("work.txt"); f:close(); return a,b,d')
        for n in (4094, 4095, 4096, 4097, 8191, 8192):
            yield c('io_buffering', f'prefill.{mode}.{n}',
                    'local f=assert(io.open("work.txt","w+")); '
                    f'f:setvbuf({quote(mode)},16); f:write("p"); '
                    f'f:write(string.rep("x",{n})); local a=#H.get("work.txt"); '
                    'f:write("q"); local b=#H.get("work.txt"); f:close(); return a,b,#H.get("work.txt")')
        for operation, action in (
                ('flush', 'f:flush()'), ('io_flush', 'io.output(f); io.flush(); io.output(before)'),
                ('seek', 'f:seek()'), ('read_zero', 'f:read(0)'), ('read_all', 'f:read("a")'),
                ('close', 'f:close()'), ('io_close', 'io.close(f)'),
                ('gc_method', 'debug.getmetatable(f).__gc(f)'),
                ('close_method', 'debug.getmetatable(f).__close(f,nil)'),
                ('switch_handle', 'io.output(f); io.output(before)'),
                ('switch_path', 'io.output(f); local g=io.output("other.txt"); io.output(before); g:close()'),
                ('newline', 'f:write("a\\nb\\nc")')):
            yield c('io_buffering', f'point.{mode}.{operation}',
                    'local before=io.output(); local f=assert(io.open("work.txt","w+")); '
                    f'f:setvbuf({quote(mode)},16); f:write("pending"); local a=H.get("work.txt"); '
                    f'{action}; local b=H.get("work.txt"); '
                    'if io.type(f)=="file" then f:close() end; return a,b,H.get("work.txt")')
        yield c('io_buffering', f'scope.{mode}',
                'do local f <close> =assert(io.open("work.txt","w")); '
                f'f:setvbuf({quote(mode)},16); f:write("scope") end; return H.get("work.txt")')
        yield c('io_buffering', f'gc.{mode}',
                'collectgarbage("collect"); collectgarbage("stop"); '
                'do local f=assert(io.open("work.txt","w")); '
                f'f:setvbuf({quote(mode)},16); f:write("gc") end; '
                'local a=H.get("work.txt"); collectgarbage("collect"); collectgarbage("collect"); '
                'collectgarbage("restart"); return a,H.get("work.txt")')

    for a in ('no', 'full', 'line'):
        for b in ('no', 'full', 'line'):
            yield c('io_buffering', f'transition.{a}.{b}',
                    'local f=assert(io.open("work.txt","w+")); '
                    f'f:setvbuf({quote(a)},16); f:write("abc"); local x=H.get("work.txt"); '
                    f'f:setvbuf({quote(b)},16); local y=H.get("work.txt"); '
                    'f:write("X"); local z=H.get("work.txt"); f:close(); return x,y,z,H.get("work.txt")')
    yield c('io_buffering', 'failed_seek_flushes',
            'local f <close> =assert(io.open("work.txt","w+")); '
            'f:write("pending"); local a=H.get("work.txt"); '
            'local p,e,n=f:seek("set",-1); return a,H.get("work.txt"),p,e,n')

    for mode in ('w', 'w+'):
        for fmt in ('bad', 'l'):
            yield c('io_buffering', f'read_validation.{mode}.{fmt}',
                    f'local f <close> =assert(io.open("work.txt",{quote(mode)})); f:write("pending"); '
                    f'local ok,e=pcall(function() return f:read({quote(fmt)}) end); '
                    'return ok,e,H.get("work.txt")')

    # Mixed newline/overflow: libc drains a whole filled prefix when the
    # write exceeds its remaining space, even with an earlier newline.
    for pre in (0, 1, 4095):
        for n in (4095, 4096, 4097, 8193):
            for newline in (0, n//2, n-1):
                yield c('io_buffering', f'line_overflow.{pre}.{n}.{newline}',
                        'local f=assert(io.open("work.txt","w+")); f:setvbuf("line",16); '
                        f'f:write(string.rep("p",{pre})); '
                        f'f:write(string.rep("x",{newline}).."\\n"..string.rep("y",{n-newline-1})); '
                        'local s=H.get("work.txt"); f:close(); return #s,s:sub(-8),#H.get("work.txt")')

    for whence in ('set', 'cur', 'end', 'bad', ''):
        for oi, offset in enumerate(('nil', '-100', '-1', '0', '2', '100', '1.5', '"2"', '{}', 'math.maxinteger', 'math.mininteger')):
            yield c('io_seek', f'{whence or "empty"}.{oi}', 'local f <close> =assert(io.open("fixtures/data.txt","r")); f:read(2); '
                    f'local r=table.pack(f:seek({quote(whence)},{offset})); H.append(r,f:seek(),f:read(2)); return table.unpack(r,1,r.n)')
    yield c('io_seek', 'default', 'local f <close> =assert(io.open("fixtures/data.txt","r")); f:read(3); return f:seek()')
    yield c('io_seek', 'past_write', 'local f <close> =assert(io.open("work.txt","w+")); f:write("a"); f:seek("set",4); f:write("z"); f:seek("set"); return f:read("a")')

    yield c('io_defaults', 'initial', 'return io.input()==io.stdin,io.output()==io.stdout,io.type(io.input()),io.type(io.output())')
    for name in ('input', 'output'):
        path = 'fixtures/data.txt' if name == 'input' else 'work.txt'
        mode = 'r' if name == 'input' else 'w'
        for switch in ('filename', 'handle'):
            expression = quote(path) if switch == 'filename' else 'f'
            yield c('io_defaults', f'{name}.{switch}', f'local before=io.{name}(); local f=assert(io.open({quote(path)},{quote(mode)})); '
                    f'local current=io.{name}({expression}); local same=io.{name}()==current; '
                    f'io.{name}(before); local t=io.type(current); if current~=f then current:close() end; f:close(); return same,t')
        for i, arg in enumerate(('nil', '{}', 'true', '"absent/child"')):
            yield c('io_defaults', f'{name}.arg.{i}', f'return io.type(io.{name}({arg}))')
        yield c('io_defaults', f'{name}.closed', f'local f=assert(io.open("fixtures/data.txt","r")); f:close(); return io.{name}(f)')
    yield c('io_defaults', 'read', 'local before=io.input(); local f=assert(io.open("fixtures/data.txt","r")); io.input(f); local r=table.pack(io.read("l","L")); io.input(before); f:close(); return table.unpack(r,1,r.n)')
    yield c('io_defaults', 'write', 'local before=io.output(); local f=assert(io.open("work.txt","w+")); io.output(f); local same=io.write("x",7)==f; local ok=io.flush(); io.output(before); f:seek("set"); local bytes=f:read("a"); f:close(); return same,ok,bytes')
    yield c('io_defaults', 'close_default', 'local before=io.output(); local f=assert(io.open("work.txt","w")); io.output(f); local ok=io.close(); io.output(before); return ok,io.type(f)')
    for name, operation in (('input','io.read()'),('output','io.flush()')):
        yield c('io_defaults', f'{name}.closed_default', f'local f=assert(io.open("work.txt","w+")); io.{name}(f); f:close(); return {operation}')

    for fmt in ('', '"l"', '"L"', '"a"', '"n"', '1', '0', '"l","l"', '"bad"'):
        for end in ('exhaust', 'break', 'error'):
            action = '' if end == 'exhaust' else ('break' if end == 'break' else 'error("loop-stop")')
            # Zero reads and read-all at EOF return empty strings indefinitely.
            guard = 'if count==8 then break end;' if fmt in ('0', '"a"') else ''
            yield c('io_lines', f'file.{formats.index(fmt) if fmt in formats else 99}.{end}',
                    f'local r=table.pack(io.lines("fixtures/lines.txt"{","+fmt if fmt else ""})); '
                    'local out={n=0}; local count=0; local ok,e=pcall(function() '
                    f'for a,b in table.unpack(r,1,r.n) do count=count+1; H.append(out,a,b); {guard}{action} end end); '
                    'H.append(out,r.n,type(r[1]),type(r[2]),type(r[3]),io.type(r[4]),count,ok,e); return table.unpack(out,1,out.n)')
    for filename in ('default', 'method'):
        yield c('io_lines', filename, 'local before=io.input(); local f=assert(io.open("fixtures/lines.txt","r")); io.input(f); '
                f'local r=table.pack({"io.lines()" if filename == "default" else "f:lines()"}); '
                'local out={n=0}; for x in table.unpack(r,1,r.n) do H.append(out,x) end; '
                'H.append(out,r.n,io.type(f),type(r[4])); io.input(before); f:close(); return table.unpack(out,1,out.n)')
    for end in ('break', 'error'):
        action = 'break' if end == 'break' else 'error("loop-stop")'
        yield c('io_lines', 'default.'+end, 'local before=io.input(); local f=assert(io.open("fixtures/lines.txt","r")); io.input(f); '
                'local r=table.pack(io.lines()); local out={n=0}; local ok,e=pcall(function() '
                f'for s in table.unpack(r,1,r.n) do H.append(out,s); {action} end end); '
                'H.append(out,r.n,type(r[4]),ok,e,io.type(f)); io.input(before); f:close(); return table.unpack(out,1,out.n)')
    for fi, fmt in enumerate(('"L"', '"n"', '2', '"l","l"', '"bad"')):
        yield c('io_lines', 'method.'+str(fi),
                'local f <close> =assert(io.open("fixtures/lines.txt","r")); local out={n=0}; '
                f'for a,b in f:lines({fmt}) do H.append(out,a,b) end; H.append(out,io.type(f)); return table.unpack(out,1,out.n)')
    for n in (249, 250, 251):
        for method in (False, True):
            yield c('io_lines', f'limit.{n}.{int(method)}', 'local f <close> =assert(io.open("fixtures/empty","r")); local a={}; '
                    f'for i=1,{n} do a[i]="l" end; local r=table.pack('
                    f'{"f:lines(table.unpack(a))" if method else "io.lines(\"fixtures/empty\",table.unpack(a))"}); '
                    'if r[4] then r[4]:close() end; return r.n,type(r[1])')
    yield c('io_lines', 'missing', 'return io.lines("missing.txt")')
    yield c('io_lines', 'iterator.closed', 'local f=assert(io.open("fixtures/data.txt","r")); local it=f:lines(); f:close(); return it()')
    yield c('io_lines', 'iterator.exhausted', 'local it,s,v,f=io.lines("fixtures/empty"); local first=table.pack(it()); local ok,e=pcall(it); H.append(first,io.type(f),ok,e); return table.unpack(first,1,first.n)')

    for i, operation in enumerate(('f:read()', 'f:write("x")', 'f:seek()', 'f:flush()', 'f:setvbuf("no")', 'f:lines()', 'f:close()', 'io.close(f)')):
        yield c('io_lifecycle', f'closed.{i}', f'local f=assert(io.open("fixtures/data.txt","r")); f:close(); return {operation}')
    for i, expression in enumerate(('nil', '{}', 'true', '1', '"file"', 'function() end', 'io.stdin')):
        yield c('io_lifecycle', f'type.{i}', f'return io.type({expression})')
    yield c('io_lifecycle', 'type.transition', 'local f=assert(io.open("fixtures/data.txt","r")); local a=io.type(f); local ok=f:close(); return a,ok,io.type(f)')
    yield c('io_lifecycle', 'tostring', 'local f=assert(io.open("fixtures/data.txt","r")); local s=tostring(f); f:close(); return type(s),s:match("^file %(")~=nil,tostring(f)')
    yield c('io_lifecycle', 'gc.explicit', 'local f=assert(io.open("fixtures/data.txt","r")); local mt=debug.getmetatable(f); local n=select("#",mt.__gc(f)); local t=io.type(f); mt.__gc(f); return n,t,io.type(f)')
    yield c('io_lifecycle', 'gc.collect', 'collectgarbage("collect"); collectgarbage("collect"); collectgarbage("stop"); '
            'local seen={n=0}; local mt=debug.getmetatable(io.stdin); local old=mt.__gc; '
            'mt.__gc=function(x) old(x); H.append(seen,io.type(x)) end; '
            '(function() local f=assert(io.open("fixtures/data.txt","r")) end)(); '
            'collectgarbage("collect"); collectgarbage("collect"); mt.__gc=old; collectgarbage("restart"); return seen.n,seen[1]')
    for terminal in ('normal', 'error'):
        action = '' if terminal == 'normal' else 'error("scope-stop")'
        yield c('io_lifecycle', f'close.{terminal}', 'local f; local ok,e=pcall(function() local x <close> =assert(io.open("fixtures/data.txt","r")); f=x; '
                f'{action} end); return ok,e,io.type(f)')
    yield c('io_lifecycle', 'close.explicit', 'local f=assert(io.open("fixtures/data.txt","r")); local mt=debug.getmetatable(f); local n=select("#",mt.__close(f,nil)); return n,io.type(f)')
    yield c('io_lifecycle', 'close.coroutine', 'local f; local co=coroutine.create(function() local x <close> =assert(io.open("fixtures/data.txt","r")); f=x; coroutine.yield("pause") end); '
            'local a,b=coroutine.resume(co); local t=io.type(f); local ok=coroutine.close(co); return a,b,t,ok,io.type(f)')
    yield c('io_lifecycle', 'tmpfile', 'local f=assert(io.tmpfile()); local t=io.type(f); local same=f:write("tmp",3)==f; f:seek("set"); local s=f:read("a"); local ok=f:close(); return t,same,s,ok,io.type(f)')
    for i, arg in enumerate(('nil', '{}', 'true', '1')):
        yield c('io_lifecycle', f'close.arg.{i}', f'return io.close({arg})')

    for stream in ('stdin', 'stdout', 'stderr'):
        yield c('io_streams', stream, f'local r=table.pack(io.close(io.{stream})); H.append(r,io.type(io.{stream})); return table.unpack(r,1,r.n)')
    yield c('io_streams', 'types', 'return io.type(io.stdin),io.type(io.stdout),io.type(io.stderr)')
    for i, expression in enumerate(('io.read("l","L","a")', 'io.stdin:read("n","l","a")', '(function() local t={}; for s in io.lines() do t[#t+1]=s end; return table.unpack(t) end)()')):
        yield c('io_stdin', i, f'return {expression}')

    for file in ('text.lua', 'multi.lua', 'broken.lua', 'runtime.lua', 'missing.lua', 'env.lua', 'shebang.lua'):
        for mode in ('nil', '"t"', '"b"', '"bt"', '"invalid"'):
            yield c('loadfile', file+'.'+mode.replace('"',''), f'local f,e=loadfile("fixtures/{file}",{mode},{{answer=91}}); '
                    'if not f then return f,e end; return f()')
        yield c('loadfile', 'dofile.'+file, f'return dofile("fixtures/{file}")')
    yield c('loadfile', 'yield', 'local co=coroutine.create(function() return dofile("fixtures/yield.lua") end); '
            'local a=table.pack(coroutine.resume(co)); local state=coroutine.status(co); H.extend(a,table.pack(coroutine.resume(co,"resume-value"))); H.append(a,state,coroutine.status(co)); return table.unpack(a,1,a.n)')
    for args in ('{}', 'true', '"fixtures/text.lua",{}'):
        yield c('loadfile', 'arg.'+str(len(args)), f'return loadfile({args})')
    for mode in ('t', 'b', 'bt', 'invalid'):
        for strip in (False, True):
            yield c('load_binary', f'{mode}.{int(strip)}', f'local dumped=string.dump(function() return 23,"binary" end,{str(strip).lower()}); '
                    f'H.put("work.bin",dumped); local f,e=loadfile("work.bin",{quote(mode)}); '
                    'if not f then return type(f),type(e),H.binary_error(e) end; return type(f),f()')
    yield c('load_binary', 'corrupt', 'H.put("work.bin",string.dump(function() return 23 end):sub(1,10)); local f,e=loadfile("work.bin","b"); return type(f),type(e),H.binary_error(e)')
    yield c('load_binary', 'dofile', 'H.put("work.bin",string.dump(function() return 23,"binary",nil end)); return dofile("work.bin")')
    yield c('loadfile_stdin', 'default', 'local f,e=loadfile(); if not f then return f,e end; return f()')
    yield c('loadfile_stdin', 'env', 'local f,e=loadfile(nil,"t",{answer=91}); if not f then return f,e end; return f()')
    yield c('dofile_stdin', 'default', 'return dofile()')

    paths = ['', ';', ';;', './?', 'modules/?.lua', ';modules/?.lua', 'modules/?.lua;',
             'missing/?.lua;;modules/?.lua', 'modules/??.lua', './?;./?;./?', 'modules/?/init.lua;modules/?.lua',
             'modules/fixed.lua', 'modules/?.lua;missing/?.lua', 'modules/?', 'modules/?.lua;;']
    names = ['good', 'missing', 'nested.item', '', 'a..b', '.good', 'good.', 'question?', 'nested/item', 'fixed']
    for pi, path in enumerate(paths):
        for ni, name in enumerate(names):
            yield c('searchpath', f'{pi}.{ni}', f'return package.searchpath({quote(name)},{quote(path)})')
    for si, (sep, rep) in enumerate((('.', '/'), ('/', '.'), ('-', '/'), ('::', '/'), ('', '/'), ('.', ''), ('.', '..'))):
        for ni, name in enumerate(('nested.item', 'nested/item', 'nested-item', 'nested::item', 'good')):
            yield c('searchpath', f'custom.{si}.{ni}',
                    f'return package.searchpath({quote(name)},"modules/?.lua",{quote(sep)},{quote(rep)})')
    for i, args in enumerate(('', 'nil,"?"', '"x",nil', '{},"?"', '"x",{}', '1,"?"', '"x","?",{}', '"x","?",".",{}')):
        yield c('searchpath', f'arg.{i}', f'return package.searchpath({args})')
    for n in (1, 20, 100):
        path = ';'.join(f'missing{i}/?.lua' for i in range(n))
        for found in (False, True):
            full = path + (';modules/?.lua' if found else '')
            yield c('searchpath', f'many.{n}.{int(found)}', f'return package.searchpath("good",{quote(full)})')

    for name in ('good', 'nested.item', 'missing', 'broken', 'runtime'):
        yield c('require', name, 'package.path="modules/?.lua;modules/?/init.lua"; package.cpath=""; '
                f'package.loaded[{quote(name)}]=nil; local r=table.pack(pcall(require,{quote(name)})); '
                'if not r[1] then return false,H.require_error(r[2]) end; local again=table.pack(require('+quote(name)+')); '
                'H.append(r,again.n,again[1]==r[2],again[2]); return table.unpack(r,1,r.n)')

    for i, op in enumerate(('os.remove("missing.txt")', 'os.rename("missing.txt","renamed.txt")',
                            'os.remove("work.txt")', 'os.rename("work.txt","renamed.txt")',
                            'os.rename("work.txt","absent/child")')):
        yield c('os_fs', i, 'H.put("work.txt","bytes"); H.remove("renamed.txt"); '
                f'local r=table.pack({op}); H.append(r,H.get("work.txt"),H.get("renamed.txt")); return table.unpack(r,1,r.n)')
    yield c('os_fs', 'tmpname', 'local s=os.tmpname(); local f=io.open(s,"r"); local exists=f~=nil; if f then f:close() end; local ok=os.remove(s); return type(s),#s>0,s:match("^/tmp/lua_")~=nil,exists,ok')
    for i, args in enumerate(('{}', 'nil', 'true')):
        yield c('os_fs', f'remove.arg.{i}', f'return os.remove({args})')
        yield c('os_fs', f'rename.arg.{i}', f'return os.rename({args},"work.txt")')

    timestamps = [-2208988800, -1, 0, 951782400, 1583650800, 1604210400, 1700000000, 2147483647]
    date_formats = ['%Y-%m-%d %H:%M:%S', '%a %A %b %B', '%c', '%d %e %j %m %w %u',
                    '%H %I %M %S %p', '%U %W %V %G %g', '%x %X %y %Y', '%z %Z',
                    '%% %n %t', '%D %F %r %R %T', '', '*t', '!*t', '%Q', '%', '%E', '%O']
    time_tables = ['{year=1970,month=1,day=1,hour=0}', '{year=2000,month=2,day=29,hour=0}',
                   '{year=2020,month=13,day=0,hour=25,min=-2,sec=70}',
                   '{year=2020,month=0,day=40,hour=-1}', '{year=2020,month=3,day=8,hour=2,min=30}',
                   '{year=2020,month=11,day=1,hour=1,min=30,isdst=false}',
                   '{year=2020,month=11,day=1,hour=1,min=30,isdst=true}',
                   '{year=2020,month=1,day=1}', '{}', '{year=2020,month=1}',
                   '{year="2020",month=1,day=1}', '{year=1.5,month=1,day=1}',
                   '{year=math.maxinteger,month=1,day=1}', '{year=2020,month={},day=1}']
    for group in ('os_utc', 'os_new_york'):
        for ti, timestamp in enumerate(timestamps):
            for fi, fmt in enumerate(date_formats if group == 'os_utc' else date_formats[:2]+['%z %Z','*t','!*t']):
                for utc in ((False, True) if group == 'os_utc' and not fmt.startswith('!') else (False,)):
                    actual = ('!' if utc else '') + fmt
                    yield c(group, f'date.{ti}.{fi}.{int(utc)}', f'local x=os.date({quote(actual)},{timestamp}); if type(x)=="table" then return H.civil(x) end; return x')
        for i, fields in enumerate(time_tables):
            # Explicit isdst resolves libc's otherwise history-dependent fold choice.
            yield c(group, f'time.{i}', f'local t={fields}; if t.isdst==nil then t.isdst=false end; local n=os.time(t); return n,H.civil(t)')
        if group == 'os_utc':
            for i, args in enumerate(('0,0', '1,0', '-1,1', '1700000000,951782400', '"3","1"', 'nil,0', '{},0')):
                yield c(group, f'diff.{i}', f'return os.difftime({args})')
            for i, args in enumerate(('"!*t",{}', '"!%Y",1.5', '{},0', '"!%Y",math.maxinteger')):
                yield c(group, f'date.arg.{i}', f'return os.date({args})')

    yield c('os_misc', 'clock', 'local x=os.clock(); return type(x),x>=0')
    yield c('os_misc', 'time_now', 'return type(os.time())')
    for name in ('MS_A', 'MS_EMPTY', 'MS_UNSET', 'TZ', 'LC_ALL'):
        yield c('os_misc', 'env.'+name, f'return os.getenv({quote(name)})')
    for i, arg in enumerate(('nil', '{}', 'true')):
        yield c('os_misc', f'env.arg.{i}', f'return os.getenv({arg})')
    for category in ('all', 'collate', 'ctype', 'monetary', 'numeric', 'time', 'bad'):
        for li, locale in enumerate(('nil', '"C"', '""', '"xx_invalid"')):
            yield c('os_misc', f'locale.{category}.{li}', f'return os.setlocale({locale},{quote(category)})')

    for i, command in enumerate(('true', 'false', 'exit 3')):
        yield c('process', f'execute.{i}', f'return os.execute({quote(command)})')
        yield c('process', f'popen.{i}', f'local f=assert(io.popen({quote(command)},"r")); local s=f:read("a"); local r=table.pack(f:close()); H.append(r,s,io.type(f)); return table.unpack(r,1,r.n)')
    yield c('process', 'execute.available', 'return os.execute()')
    yield c('process', 'popen.echo', 'local f=assert(io.popen("echo fixture","r")); local s=f:read("a"); local r=table.pack(f:close()); H.append(r,s); return table.unpack(r,1,r.n)')
    yield c('process', 'popen.write', 'local f=assert(io.popen("cat > pipe.txt","w")); local same=f:write("pipe-bytes")==f; local r=table.pack(f:close()); H.append(r,same,H.get("pipe.txt")); return table.unpack(r,1,r.n)')
    for mode in ('x', 'rb', 'r+', ''):
        yield c('process', 'popen.mode.'+(mode or 'empty'), f'return io.popen("true",{quote(mode)})')
    yield c('arg', 'layout', 'return arg[-2],arg[-1],arg[0],arg[1],arg[2],arg[3],arg[4],#arg')
    yield c('arg', 'varargs', 'local r=H.argv; return table.unpack(r,1,r.n)')


DRIVER = r'''local argv=table.pack(...)
local function hex(s)
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
  error('unsupported protocol value: '..t)
end
local H={argv=argv}
function H.append(t,...) local r=table.pack(...); for i=1,r.n do t.n=t.n+1; t[t.n]=r[i] end end
function H.extend(t,r) for i=1,r.n do t.n=t.n+1; t[t.n]=r[i] end end
function H.put(p,s) local f <close> =assert(io.open(p,'wb')); assert(f:write(s)) end
function H.get(p) local f=io.open(p,'rb'); if not f then return nil end; local s=f:read('a'); f:close(); return s end
function H.remove(p) os.remove(p) end
function H.civil(t) return t.year,t.month,t.day,t.hour,t.min,t.sec,t.wday,t.yday,t.isdst end
function H.binary_error(e)
  if e:find('mode',1,true) then
    if e:find('binary',1,true) then return 'mode:binary' end
    if e:find('text',1,true) then return 'mode:text' end
  end
  if e:find('binary',1,true) or e:find('bytecode',1,true) or e:find('version',1,true) then return 'invalid-binary' end
  return 'other-error'
end
function H.require_error(e)
  if not e:find("not found:",1,true) then return e end
  local lines={}; for s in e:gmatch('[^\n]+') do
    if #lines==0 or s:find('no field package.preload',1,true) or s:find("no file 'modules/",1,true) then lines[#lines+1]=s end
  end
  return table.concat(lines,'\n')
end
local function execute(c)
  local input,output
  if io then input=io.input(); output=io.output() end
  local path,cpath=package.path,package.cpath
  local f,e=load(c[2],'@hostlib-case.lua')
  local status,r,err='compile',{n=0},e
  if f then
    r=table.pack(pcall(f,H))
    if r[1] then status='ok'; err=nil
    else status='error'; err=r[2]; r={n=0} end
  end
  if io then io.input(input); io.output(output) end
  package.path=path; package.cpath=cpath
  local n=status=='ok' and r.n-1 or 0
  local fields={c[1],status,tostring(n)}
  for i=1,n do local t,b=value(r[i+1]); fields[#fields+1]=t; fields[#fields+1]=b end
  if status=='ok' then fields[#fields+1]='-'; fields[#fields+1]='-'
  else local t,b=value(err); fields[#fields+1]=t; fields[#fields+1]=b end
  print(table.concat(fields,'\t'))
end
local cases={%s}
for _,c in ipairs(cases) do execute(c) end
'''


def driver(cases):
    rows = ['{' + quote(c['id']) + ',' + quote(c['source']) + '}' for c in cases]
    return DRIVER.replace('{%s}', '{' + ','.join(rows) + '}').encode('ascii')


def grouped_batches():
    groups = collections.defaultdict(list)
    seen = set()
    for item in all_cases():
        if item['id'] in seen:
            raise ValueError('duplicate case: ' + item['id'])
        seen.add(item['id'])
        groups[item['group']].append(item)
    for group, items in groups.items():
        size = 1 if group in {'io_stdin', 'io_streams', 'loadfile_stdin', 'dofile_stdin'} else BATCH_SIZE
        for start in range(0, len(items), size):
            yield f'{group}-{start//size:04d}', items[start:start+size]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    with (args.out / 'cases.jsonl').open('w') as stream:
        for name, cases in grouped_batches():
            (args.out / (name + '.lua')).write_bytes(driver(cases))
            for item in cases:
                stream.write(json.dumps(item, sort_keys=True) + '\n')


if __name__ == '__main__':
    main()
