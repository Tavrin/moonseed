#!/usr/bin/env python3
"""Deterministic hook case definitions and the shared pure-Lua driver."""
from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
HOOKS = ROOT / 'tests' / 'hooks'


def case(group, name, source, mask=None, count=0, oracle_only=False):
    return dict(group=group, id=f'{group}.{name}', chunk='@hooks.lua', source=source,
                mask=mask, count=count, oracle_only=oracle_only)


def all_cases():
    out = []

    def add(group, name, source, mask=None, count=0, oracle_only=False):
        out.append(case(group, name, source, mask, count, oracle_only))

    calls = {
        'ordinary': 'local function f(a,b) return a,b,nil end\nprint(f(1,"x"))',
        'nested': 'local function f(a) local function g(b) return b+1 end; return g(a)+1 end\nprint(f(3))',
        'method': 'local o={x=7}; function o:m(a) return self.x+a end\nprint(o:m(2))',
        'sort': 'local t={3,1,2}; table.sort(t,function(a,b) return a<b end); print(table.concat(t,","))',
        'gsub': 'print(string.gsub("ab", ".", function(s) return s..s end))',
        'pcall': 'print(pcall(function(a) return a,nil,3 end,1))',
        'xpcall': 'print(xpcall(function() error("body",0) end,function(e) return "handled:"..e end))',
        'varargs': 'local function f(a,...) return a,... end\nprint(f(1,nil,"x",false))',
        'tail_lua': 'local function g(a) return a,nil,3 end; local function f(a) return g(a) end; print(f(2))',
        'deep_tail': 'local function f(n) if n==0 then return 9 end; return f(n-1) end; print(f(128))',
        'tail_builtin': 'local function f(...) return select(2,...) end; print(f(1,nil,"x",4))',
        'zero_results': 'local function f() end; print(f())',
        'many_results': 'local function f(a,b,c) return nil,a,b,c,nil end; print(f(1,false,"x"))',
        'native_sin': 'print(math.sin(0))',
        'native_select': 'print(select(2,1,nil,"x",false))',
        'native_unpack': 'print(table.unpack({1,2,3}))',
        'native_base': 'print(type(nil),tonumber("17"),rawlen({1,2}),rawequal(1,1))',
        'native_string': 'print(string.sub("abc",2),string.byte("a"),string.find("abc","b"),string.format("%d",4))',
        'native_table': 'local t={1}; table.insert(t,2); print(table.remove(t),table.concat(t))',
        'stripped': 'local h,m,c=debug.gethook(); debug.sethook(); local f=assert(load("return function(a,...) return a,... end", "@strip.lua"))(); local g=assert(load(string.dump(f,true))); debug.sethook(h,m,c); print(g(1,nil,3))',
    }
    for name, src in calls.items():
        add('calls', name, src, 'crl')
    metas = {
        'index': ('__index=function(t,k) return k.."!" end', 'print(a.x)'),
        'add': ('__add=function(a,b) return 7 end', 'print(a+2)'),
        'call': ('__call=function(t,a) return a,nil end', 'print(a(3))'),
        'eq': ('__eq=function(a,b) return true end', 'local b=setmetatable({},mt); print(a==b)'),
        'lt': ('__lt=function(a,b) return true end', 'print(a<{})'),
        'concat': ('__concat=function(a,b) return "joined" end', 'print(a.."x")'),
    }
    for name, (mt, body) in metas.items():
        add('metamethods', name, f'local mt={{{mt}}}; local a=setmetatable({{}},mt); {body}', 'crl')
    closes = {
        'return': 'local function f() local a <close> = setmetatable({},mt); return 1,nil,3 end; print(f())',
        'error': 'print(pcall(function() local a <close> = setmetatable({},mt); error("body",0) end))',
        'goto': 'do local a <close> = setmetatable({},mt); goto done end; ::done:: print("done")',
        'coroutine_close': 'local co=coroutine.create(function() local a <close> = setmetatable({},mt); coroutine.yield("pause") end); debug.sethook(co,H,"crl"); print(coroutine.resume(co)); print(coroutine.close(co))',
    }
    for name, body in closes.items():
        add('close_gc', name, 'local mt={__close=function(_,e) print("close",e) end}; '+body, 'crl')
    add('close_gc', 'gc_suppressed', 'local n=0; do local t=setmetatable({},{__gc=function() n=n+1; local function f() print("gc") end; f() end}) end; collectgarbage("collect"); print("finalized",n)', 'crl')
    coroutines = {
        'resume_yield': 'local co=coroutine.create(function(a) local b=coroutine.yield(a,nil); return b end); debug.sethook(co,H,"crl"); print(coroutine.resume(co,1)); print(coroutine.resume(co,2))',
        'wrap': 'local w=coroutine.wrap(function() debug.sethook(H,"crl"); coroutine.yield(1); return 2 end); print(w()); print(w())',
        'suspended_install': 'local co=coroutine.create(function() local x=1; coroutine.yield(x); x=x+1; return x end); print(coroutine.resume(co)); debug.sethook(co,H,"crl"); print(coroutine.resume(co))',
        'different_threads': 'local function body() coroutine.yield(1); return 2 end; local a,b=coroutine.create(body),coroutine.create(body); debug.sethook(a,H,"c"); debug.sethook(b,H,"r"); print(coroutine.resume(a)); print(coroutine.resume(b)); print(coroutine.resume(a)); print(coroutine.resume(b)); print(debug.gethook(a)); print(debug.gethook(b))',
        'noninheritance': 'local co=coroutine.create(function() print("child",debug.gethook()); local function f() return 1 end; return f() end); print("childhook",debug.gethook(co)); print(coroutine.resume(co))',
    }
    for name, src in coroutines.items():
        add('coroutines', name, src, 'crl')
    controls = {
        'if_elseif': 'local x=2\nif x==1 then\n print(1)\nelseif x==2 then\n print(2)\nelse\n print(3)\nend\nreturn x',
        'while': 'local n=0\nwhile n<3 do\n n=n+1\nend\nprint(n)',
        'repeat': 'local n=0\nrepeat\n n=n+1\nuntil n==3\nprint(n)',
        'numeric_for': 'local n=0\nfor i=1,3 do\n n=n+i\nend\nprint(n)',
        'numeric_for_negative': 'local n=0\nfor i=3,1,-1 do\n n=n+i\nend\nprint(n)',
        'generic_for': 'local n=0\nfor k,v in ipairs({1,2,3}) do\n n=n+v\nend\nprint(n)',
        'break': 'local n=0\nwhile true do\n n=n+1\n if n==2 then break end\nend\nprint(n)',
        'goto_forward': 'local n=1\ngoto done\nn=9\n::done::\nprint(n)',
        'goto_backward': 'local n=0\n::again::\nn=n+1\nif n<3 then goto again end\nprint(n)',
        'same_line_backward': 'local n=0; ::again:: n=n+1; if n<3 then goto again end; print(n)',
        'and_or': 'local x=false\nlocal y=x and\n 9 or\n 3\nprint(y)',
        'and_taken': 'local x=true\nlocal y=x and\n 9 or\n 3\nprint(y)',
        'multiline_call': 'local function f(a,b)\n return a+b\nend\nprint(f(\n 1,\n 2\n))',
        'multiline_expression': 'local x=(1\n +2)\n *3\nprint(x)',
        'constructor': 'local t={\n 1,\n x=2,\n [3]=4\n}\nprint(t[1],t.x,t[3])',
        'returns': 'local function f(a)\n if a then\n  return\n   1,2\n end\n return 3\nend\nprint(f(true),f(false))',
        'empty_loop': 'for i=1,3 do end\nprint("done")',
        'local_declaration': 'local x=1\nlocal function f() return x end\nprint(f())',
    }
    for name, src in controls.items():
        add('lines', name, src, 'l')
    # API cases avoid incidental instruction-count traces.
    forms = {
        'no_args': 'debug.sethook(); print(debug.gethook())',
        'nil': 'debug.sethook(H,"crl"); debug.sethook(nil); print(debug.gethook())',
        'thread_only': 'local co=coroutine.create(function() end); debug.sethook(co,H,"crl"); debug.sethook(co); print(debug.gethook(co))',
        'thread_nil': 'local co=coroutine.create(function() end); debug.sethook(co,nil); print(debug.gethook(co))',
        'replace': 'local co=coroutine.create(function() end); debug.sethook(co,H,"c"); local f=function() end; debug.sethook(co,f,"rl",4); local h,m,n=debug.gethook(co); print(h==f,m,n)',
        'gethook_no_args': 'print(debug.gethook())',
        'gethook_bad_thread': 'print(debug.gethook(17))',
        'bad_hook_number': 'debug.sethook(17,"c")',
        'bad_hook_boolean': 'debug.sethook(true,"c")',
        'bad_hook_table': 'debug.sethook({},"c")',
        'callable_table': 'debug.sethook(setmetatable({},{__call=function() end}),"c")',
        'missing_mask': 'debug.sethook(H)',
        'bad_mask_table': 'debug.sethook(H,{})',
        'numeric_mask': 'debug.sethook(H,123); print(debug.gethook()); debug.sethook()',
    }
    for name, src in forms.items():
        add('api', name, src)
    for index, mask in enumerate(['', 'xyz', 'rlc', 'ccccrrll', 'c?l', 'tail call', 'c\x00rl']):
        add('api', f'mask_{index}', 'local co=coroutine.create(function() end); debug.sethook(co,H,'+lua_quote(mask)+'); local h,m,n=debug.gethook(co); print(h==H,m,n)')
    for name, n in [('zero','0'),('negative','-1'),('minint','math.mininteger'),('one','1'),('two','2'),('four','4'),('hundred','100'),('four_thousand','4000'),('max24','2^24-1'),('huge','math.maxinteger'),('wrap32','2^32+1'),('float','1.5'),('integral_float','2.0'),('string','"4"'),('bad_string','"x"'),('nil','nil')]:
        add('api', 'count_'+name, 'local co=coroutine.create(function() end); debug.sethook(co,H,"",'+n+'); local h,m,n=debug.gethook(co); print(h==H,m,n)')
    reentrant = {
        'calls_lua': 'local function f() return 7 end; print("hooklua",f())',
        'getinfo': 'local i=debug.getinfo(3,"nSltur"); print("inspect",i.what,i.currentline)',
        'pcall': 'print("hookpcall",pcall(function() error("inside",0) end))',
        'metamethod': 'local t=setmetatable({},{__add=function() return 8 end}); print("hookmeta",t+1)',
        'native': 'print("hooknative",math.sin(0),string.sub("abc",2))',
        'long_body': 'local s=0; for i=1,1000 do s=s+i end; print("hooklong",s)',
        'replace': 'debug.sethook(K(function(e) print("replacement",e); debug.sethook() end),"l")',
        'disable': 'debug.sethook(); print("disabled")',
        'raises': 'error("hook-error",0)',
    }
    for name, body in reentrant.items():
        add('reentrancy', name, 'local once=false; debug.sethook(K(function(e,l) if not once then once=true; '+body+' end end),"c"); local function f() return 1 end; print(pcall(f)); debug.sethook(); print("after")')
    for event, mask, count in [('call','c',0),('return','r',0),('tail call','c',0),('line','l',0),('count','',1)]:
        for path in ['pcall','xpcall','resume','wrap']:
            body = 'local function g() return 4 end; local function f() return g() end; print(f())'
            hook = 'K(function(e,l,i) if e=='+lua_quote(event)+' and i.short_src=="victim.lua" then error("hook-'+event+'",0) end end)'
            victim = 'assert(load('+lua_quote(body)+',"@victim.lua", "t", _ENV))'
            if path in ['pcall','xpcall']:
                invocation = 'pcall(f)' if path=='pcall' else 'xpcall(f,function(e) return "handled:"..e end)'
                src = 'local f='+victim+'; debug.sethook('+hook+','+lua_quote(mask)+','+str(count)+'); local ok,e='+invocation+'; local h,m,c=debug.gethook(); print("installed",type(h),m,c); debug.sethook(); print(ok,e); print("recovered",pcall(f))'
            else:
                src = 'local f='+victim+'; local co=coroutine.create(f); debug.sethook(co,'+hook+','+lua_quote(mask)+','+str(count)+'); '
                if path=='resume':
                    src += 'print(coroutine.resume(co)); print("stillhook",debug.gethook(co)); debug.sethook(co); print(coroutine.status(co))'
                else:
                    # wrap creates its own thread: install there before entering the fixed victim.
                    src = 'local f='+victim+'; local w=coroutine.wrap(function() debug.sethook('+hook+','+lua_quote(mask)+','+str(count)+'); return f() end); print(pcall(w))'
            add('count_positions' if event=='count' else 'errors', event.replace(' ','_')+'_'+path, src, oracle_only=event=='count')
        # Lua hooks are non-yieldable at every event kind.
        src = 'local co=coroutine.create(function() local function g() return 1 end; return g() end); debug.sethook(co,K(function(e) if e=='+lua_quote(event)+' then coroutine.yield("illegal") end end),'+lua_quote(mask)+','+str(count)+'); print(coroutine.resume(co)); debug.sethook(co)'
        add('count_positions' if event=='count' else 'lua_yield', event.replace(' ','_'), src, oracle_only=event=='count')
    count_cases = {
        'fires_stops': 'local fired=false; debug.sethook(function() fired=true end,"",1); local x=1+2; debug.sethook(); local before=fired; fired=false; for i=1,10 do x=x+i end; print(before,fired,x)',
        'body_suppression': 'local active,nested=false,false; local fired=false; debug.sethook(function() if active then nested=true end; active=true; fired=true; local x=0; for i=1,100 do x=x+i end; active=false end,"",1); local x=1+2; debug.sethook(); print(fired,nested)',
        'reset': 'local a,b=false,false; debug.sethook(function() a=true end,"",1); local x=1; debug.sethook(function() b=true end,"",1); x=x+1; debug.sethook(); print(a,b,x)',
        'disable_inside': 'local fired=false; debug.sethook(function() fired=true; debug.sethook() end,"",1); local x=1; print(fired,debug.gethook())',
        'replace_inside': 'local a,b=false,false; debug.sethook(function() a=true; debug.sethook(function() b=true end,"",1) end,"",1); local x=1; debug.sethook(); print(a,b)',
    }
    for name, src in count_cases.items():
        add('count', name, src)
    for name, n in [('zero',0),('negative',-1),('one',1),('two',2),('four',4),('hundred',100),('four_thousand',4000),('max24',2**24-1)]:
        add('count', 'interval_'+name, 'local co=coroutine.create(function() local n=0; for i=1,1000 do n=n+i end; return n end); local fired=false; local h=function() fired=true end; debug.sethook(co,h,"",'+str(n)+'); local got,m,c=debug.gethook(co); print(got==h,m,c); print(coroutine.resume(co)); debug.sethook(co); '+('print(fired)' if n in [0,-1,1,2,4] else 'print("positions-reported-separately")'))
    for name, src in controls.items():
        add('count_positions', 'loop_'+name, src, '', 2, True)
    for n in [1,2,4,100,4000,2**24-1]:
        add('count_positions', 'interval_'+str(n), 'local x=0; for i=1,40 do x=x+i end; print(x)', '', n, True)
    add('count_positions', 'count_line_order', 'local x=0\nfor i=1,3 do\n x=x+i\nend\nprint(x)', 'l', 1, True)
    add('count_positions', 'long_hook_counter', 'local first=true; debug.sethook(K(function(e,l) if first then first=false; local x=0; for i=1,100 do x=x+i end end end),"l",4); local x=0\nfor i=1,3 do x=x+i end\ndebug.sethook(); print(x)', oracle_only=True)
    add('count_positions', 'sethook_counter_reset', 'debug.sethook(H,"l",4); local x=1; debug.sethook(H,"l",4); x=x+1; debug.sethook(); print(x)', oracle_only=True)
    external = {
        'gethook': 'print(chookget()); local co=coroutine.create(function() return 1 end); local r=chook(co,"rlc",4,"none"); print(chookget(co)); print(debug.gethook(co)); chookoff(co); print(chookget(co)); local f=function() end; debug.sethook(co,f,"c"); local h,m,n=chookget(co); print(h==f,m,n); chookoff(co); C(r)',
        'line_resume': 'local co=coroutine.create(function()\n local x=1\n x=x+2\n x=x*3\n return x\nend); local r=chook(co,"l",0,"line"); D(co); chookoff(co); C(r)',
        'line_inspect_edit': 'local co=coroutine.create(function(...)\n local x=1\n x=x+2\n return x,...\nend); local r=chook(co,"l",0,"line"); print(coroutine.resume(co,7,nil)); print(coroutine.resume(co)); local i=debug.getinfo(co,0,"nSltur"); print(i.what,i.currentline,i.istailcall); local n,v=debug.getlocal(co,0,1); print(n,v); print(debug.setlocal(co,0,1,10)); D(co); chookoff(co); C(r)',
        'call_yield_attempt': 'local co=coroutine.create(function() local function f() return 3 end; return f() end); local r=chook(co,"c",0,"call"); D(co); chookoff(co); C(r)',
        'return_yield_attempt': 'local co=coroutine.create(function() local function f() return 3 end; local x=f(); return x+1 end); local r=chook(co,"r",0,"return"); D(co); chookoff(co); C(r)',
        'line_main_yield_attempt': 'local r; local ok,e=xpcall(function() r=chook("l",0,"line")\n local x=1\n return x end,function(e) chookoff(); return e end); print(ok,e); if r then C(r) end',
        'ordinary_transfers': 'local co=coroutine.create(function(a) local function f(x) return x,nil,4 end; return f(a) end); local r=chook(co,"cr",0,"none"); print(coroutine.resume(co,3)); chookoff(co); C(r)',
    }
    for name, src in external.items():
        add('external_unsupported' if name in ['call_yield_attempt','return_yield_attempt'] else 'external', name, src, oracle_only=name in ['call_yield_attempt','return_yield_attempt'])
    for n in [1,2]:
        add('external', 'count_semantics_'+str(n), 'local co=coroutine.create(function() local function f(a) return a+1 end; local t={f(1),f(2),f(3)}; return table.unpack(t) end); local r=chook(co,"c",'+str(n)+',"count"); local yielded=false; local result; repeat result=table.pack(coroutine.resume(co)); if coroutine.status(co)~="dead" then yielded=true end until coroutine.status(co)=="dead" or not result[1]; chookoff(co); print(table.unpack(result,1,result.n)); local calls=0; for _,v in ipairs(r) do if v.event=="call" and v.what=="Lua" then calls=calls+1 end end; print("yielded",yielded,"lua-calls",calls)')
    for n in [1,2]:
        for name, mask, mode in [('count_resume','', 'count'),('count_call_no_duplicate','c','count'),('count_line','l','count'),('both','l','both')]:
            add('external_positions', name+'_'+str(n), 'local co=coroutine.create(function() local function f(a) return a+1 end; local t={f(1),f(2),f(3)}; return table.unpack(t) end); local r=chook(co,'+lua_quote(mask)+','+str(n)+','+lua_quote(mode)+'); D(co); chookoff(co); C(r)', oracle_only=True)
    # Review regressions: ordinary native stack layouts and multiline reads.
    for name, source in {
        'raw_access': 'local t={a=1}; print(rawget(t,"a")); print(rawset(t,"b",2)==t)',
        'next_returns': 'local t={a=1}; print(next(t)); print(next(t,"a"))',
        'reader_load': 'local i=0; local f=load(function() i=i+1; if i==1 then return "return 1" end end); print(f())',
        'gmatch_hook': 'debug.sethook(string.gmatch("xxx","."),"crl"); local a=1; local function f() return a end; f(); debug.sethook(); print(a)',
        'pack_returns': 'print(string.unpack("i4",string.pack("i4",1)))',
        'debug_metatable': 'local t={}; print(debug.setmetatable(t,{})==t)',
    }.items():
        add('calls', 'review_'+name, source, 'crl')
    for name, source in {
        'field': 'local t={x=2}\nlocal x=t\n.x\nprint(x)',
        'and_taken': 'local t={x=true}\nlocal x=t.x\nand\nt.x\nprint(x)',
        'and_skipped': 'local t={x=false}\nlocal x=t.x\nand\nt.x\nprint(x)',
        'or_taken': 'local t={x=false}\nlocal x=t.x\nor\nt.x\nprint(x)',
        'or_skipped': 'local t={x=true}\nlocal x=t.x\nor\nt.x\nprint(x)',
    }.items():
        add('lines', 'review_'+name, source, 'crl')
    ids = [c['id'] for c in out]
    if len(ids) != len(set(ids)):
        raise ValueError('duplicate hook case id')
    return out


def lua_quote(value):
    return '"' + ''.join(f'\\{b:03d}' for b in value.encode('utf-8')) + '"'


# Case table is exactly one source line, so driver locations never depend on batch size.
DRIVER = r'''
local rawprint=print
local function hex(s)
 local t={}; for i=1,#s do t[i]=string.format('%02x',string.byte(s,i)) end; return table.concat(t)
end
local function execute(c)
 local records,ids,nextid={}, {},0
 local function value(v)
  local t=type(v)
  if t=='nil' then return 'nil' end
  if t=='string' then return 'string:'..hex(v) end
  if t=='number' then return 'number:'..string.format('%.17g',v) end
  if t=='boolean' then return 'boolean:'..tostring(v) end
  if not ids[v] then nextid=nextid+1; ids[v]=nextid end
  return t..':'..ids[v]
 end
 local function emit(kind,s)
  if #records>=20000 then error('Lua hook record limit',0) end
  records[#records+1]={kind,s}
 end
 local fields={'name','namewhat','what','short_src','currentline','linedefined','istailcall','ftransfer','ntransfer','nparams','isvararg'}
 local function event(e,l,i,transfers,depth)
  local t={value(e),value(l)}
  for _,k in ipairs(fields) do t[#t+1]=value(i[k]) end
  t[#t+1]='depth:'..depth
  for _,v in ipairs(transfers) do t[#t+1]=value(v.name)..'='..value(v.value) end
  emit('H',table.concat(t,'|'))
 end
 local function K(callback) return function(e,l)
  local i=debug.getinfo(2,'nSltur')
  local transfers={}
  if e=='call' or e=='return' then
   for k=0,i.ntransfer-1 do local n,v=debug.getlocal(2,i.ftransfer+k); transfers[#transfers+1]={name=n,value=v} end
  end
  local depth=0; while debug.getinfo(depth+2,'') do depth=depth+1 end
  event(e,l,i,transfers,depth)
  if callback then callback(e,l,i) end
 end end
 local H=K()
 local env={}; for k,v in pairs(_G) do env[k]=v end; env._G=env
 env.H=H; env.K=K
 env.print=function(...)
  local t={}; for k=1,select('#',...) do
   local v=select(k,...); local typ=type(v)
   if typ=='string' or typ=='number' or typ=='boolean' or typ=='nil' then t[k]=tostring(v) else t[k]=value(v) end
  end
  emit('O',table.concat(t,'\t')..'\n')
 end
 env.C=function(rows) for _,r in ipairs(rows) do event(r.event,r.line,r,r.transfers,r.depth) end end
 env.D=function(co)
  local n=0
  repeat
   n=n+1; if n>20000 then error('resume limit',0) end
   local r=table.pack(coroutine.resume(co)); env.print(table.unpack(r,1,r.n))
   if not r[1] then break end
  until coroutine.status(co)=='dead'
 end
 local f,err=load(c[2],c[3],'t',env)
 local result
 if f then
  if c[4]~=false then debug.sethook(H,c[4],c[5]) end
  result=table.pack(pcall(f))
  if debug.sethook then debug.sethook() end
 else result={false,err,n=2} end
 local status=not f and 'compile' or (result[1] and 'ok' or 'error')
 local t={status}; for k=2,result.n do t[#t+1]=value(result[k]) end
 emit('S',table.concat(t,'|'))
 for seq,r in ipairs(records) do rawprint(c[1]..'\t'..seq..'\t'..r[1]..'\t'..hex(r[2])) end
end
local cases = {CASES}
for _,c in ipairs(cases) do execute(c) end
'''


def driver(cases):
    rows = ['{'+','.join([lua_quote(c['id']), lua_quote(c['source']), lua_quote(c['chunk']),
                         'false' if c['mask'] is None else lua_quote(c['mask']), str(c['count'])])+'}' for c in cases]
    return DRIVER.replace('CASES', ','.join(rows)).encode('ascii')


def grouped_batches():
    groups = {}
    for c in all_cases():
        groups.setdefault(c['group'], []).append(c)
    yield from sorted(groups.items())


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    for name, cases in grouped_batches():
        (args.out / (name+'.lua')).write_bytes(driver(cases))
    (args.out / 'manifest.json').write_text(json.dumps(all_cases(), indent=2)+'\n')
