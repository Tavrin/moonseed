"""Deterministic Lua 5.4 diagnostic cases and a shared PUC/Moonseed driver.

Case records are plain data (id, chunk name, source, optional setup). No case
depends on a host-specific Lua library. Run diag_corpus.py to materialize the
drivers and capture or compare their byte-exact protocol output.
"""

from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DIAG = ROOT / "tests" / "diag"


def case(group, name, src, chunk="@diag.lua", setup=""):
    return dict(group=group, id=f"{group}.{name}", source=src, chunk=chunk, setup=setup)


def handwritten():
    out = []

    def add(group, name, src, chunk="@diag.lua", setup=""):
        out.append(case(group, name, src, chunk, setup))

    # Location and operand provenance. Every expression is compiled at the
    # named site, including local, upvalue, global, field, and temporaries.
    values = {
        "nil": "nil", "boolean": "true", "number": "1", "string": "'abc'",
        "table": "{}", "function": "function() end",
        "thread": "coroutine.create(function() end)",
    }
    source_forms = {
        "local": "local victim = {v}\n{op}",
        "upvalue": "local victim = {v}\nlocal function inner() {op} end\ninner()",
        "global": "diag_victim = {v}\n{op}",
        "field": "local holder = {{victim = {v}}}\n{op}",
        "temporary": "local function make() return {v} end\n{op}",
        "constant": "{op}",
    }
    references = {
        "local": "victim", "upvalue": "victim", "global": "diag_victim",
        "field": "holder.victim", "temporary": "make()", "constant": "({v})",
    }
    operations = {
        "call": "{x}()", "index": "local z = {x}.key",
        "index_assign": "{x}.key = 1", "add": "local z = {x} + 2",
        "bitand": "local z = {x} & 2", "unm": "local z = -{x}",
        "len": "local z = #{x}", "concat": "local z = {x} .. {}",
        "lt": "local z = {x} < 2",
    }
    for form, template in source_forms.items():
        for typ, value in values.items():
            ref = references[form].replace("{v}", value)
            for op, expr in operations.items():
                if (op == "call" and typ == "function") or (op.startswith("index") and typ == "table"):
                    continue
                if op in {"add", "bitand", "unm"} and typ == "number":
                    continue
                if op == "len" and typ in {"string", "table"}:
                    continue
                if op == "lt" and typ == "number":
                    continue
                if op == "index" and typ == "string":
                    expr = "local z = {x}.key.q"
                src = template.format(v=value, op=expr.replace("{x}", ref))
                add("runtime", f"{form}.{typ}.{op}", src)

    nested = {
        "chain": "local a = {b={c={d=nil}}}; local z = a.b.c.d.key",
        "call_chain": "local function f() return {x={y=nil}} end; local z=f().x.y.key",
        "or": "local a=nil; local b=nil; local z=(a or b).key",
        "indexed": "local t={}; local k='missing'; local z=t[k].key",
        "numeric": "local t={}; local z=t[1].key",
        "method": "local t={get=function() return nil end}; local z=t:get().key",
    }
    for name, src in nested.items():
        add("runtime", "nested." + name, src)
    multiline = [
        "local f=nil\nf(\n  1,\n  2)",
        "local a={}\nlocal z = a\n  +\n  true",
        "local t={}\nlocal z={\n t.missing.x\n}",
        "local t={}\nif t.missing.x\n then end",
        "local t={}\nreturn\n t.missing.x",
        "local t={}\nlocal z=t:\n missing()",
        "local a=nil\nlocal z = (a\n  or false).x",
        "local t={}\nlocal z = t[\n  'x'].y",
    ]
    for i, src in enumerate(multiline):
        add("runtime", f"multiline.{i:02}", src)
    for op, expr in {
        "sub": "x - 1", "mul": "x * 1", "div": "x / 1", "idiv": "x // 1",
        "mod": "x % 1", "pow": "x ^ 1", "bitor": "x | 1",
        "bitxor": "x ~ 1", "shl": "x << 1", "shr": "x >> 1",
        "bitnot": "~x", "le": "x <= 1", "gt": "x > 1", "ge": "x >= 1",
    }.items():
        add("runtime", f"extra.{op}", "local x = {}\nlocal z = " + expr)
    for name, src in {
        "for_initial": "for i=nil,3 do end",
        "for_limit": "for i=1,{} do end",
        "for_step": "for i=1,3,{} do end",
        "integer_conversion": "local x=1.5; local z=x & 1",
        "arith_coercion": "local x='not a number'; local z=x+1",
        "method_call": "local x={}; x:missing()",
    }.items():
        add("runtime", name, src)

    levels = (0, 1, 2, 3, 1000)
    messages = {"string": "'boom'", "number": "42", "false": "false", "nil": "nil", "table": "_diag_marker"}
    paths = {
        "direct": "error({msg}, {level})",
        "lua": "local function inner() error({msg}, {level}) end\ninner()",
        "pcall": "local function inner() error({msg}, {level}) end\nlocal ok,e=pcall(inner); error(e,0)",
        "xpcall": "local function inner() error({msg}, {level}) end\nlocal ok,e=xpcall(inner,function(e) return e end); error(e,0)",
        "resume": "local co=coroutine.create(function() error({msg},{level}) end)\nlocal ok,e=coroutine.resume(co); error(e,0)",
        "wrap": "local f=coroutine.wrap(function() error({msg},{level}) end)\nf()",
        "tail": "local function inner() return error({msg},{level}) end\nreturn inner()",
        "sort": "table.sort({2,1},function() error({msg},{level}) end)",
        "gsub": "string.gsub('a','a',function() error({msg},{level}) end)",
    }
    for path, template in paths.items():
        for typ, message in messages.items():
            for level in levels:
                setup = "_diag_marker = {}" if typ == "table" else ""
                add("error", f"{path}.{typ}.level{level}", template.replace("{msg}", message).replace("{level}", str(level)), setup=setup)
    for name, src in {
        "bare": "error('boom')", "plain": "error('boom', 0)",
        "bad_level": "error('boom', {})", "level_nil": "error('boom', nil)",
        "assert": "assert(false)", "assert_string": "assert(false, 'bad')",
        "assert_number": "assert(false, 42)", "assert_nil": "assert(false, nil)",
        "assert_true_ok": "assert(true)",
    }.items():
        add("error", name, src)

    arg_calls = {
        "global": "local w=string.rep; string.rep({args})",
        "local": "local w=string.rep; w({args})",
        "field": "local t={w=string.rep}; t.w({args})",
        "method": "local s='abc'; s:rep({args})",
        "metamethod": "local t=setmetatable({}, {__call=string.rep}); t({args})",
        "builtin": "string.rep({args})",
        "closure": "local function make() local f=string.rep; return function(...) return f(...) end end; local w=make(); w({args})",
        "mutated": "local w=string.rep; string.rep=function() end; local ok,e=pcall(function() return w({args}) end); string.rep=w; if not ok then error(e,0) end",
        "tail": "local function w(...) return string.rep(...) end; w({args})",
        "tail_two_levels": "local function w(...) return string.rep(...) end; local function outer(...) return w(...) end; outer({args})",
    }
    for name, template in arg_calls.items():
        for kind, args in {"missing": "", "wrong": "{}", "bad_self": "false, 2"}.items():
            add("argument", f"rep.{name}.{kind}", template.replace("{args}", args))
    arg_samples = {
        "base": ["tonumber()", "tonumber({},2)", "select()", "rawget()", "rawset({},nil,1)", "setmetatable(1,{})", "collectgarbage('invalid')", "load({})"],
        "math": ["math.abs()", "math.max()", "math.random(1,2,3)", "math.tointeger({})", "math.floor({})"],
        "table": ["table.concat()", "table.concat({}, {})", "table.insert()", "table.remove(1)", "table.sort({2,1}, 1)", "table.move({}, 1, 2, 3, false)"],
        "string": ["string.byte()", "string.char({})", "string.find()", "string.format('%d', {})", "string.gsub()", "string.match({},'a')", "string.pack('i', {})", "string.rep('x', {})", "string.sub('x')"],
        "coroutine": ["coroutine.resume()", "coroutine.status()", "coroutine.wrap()", "coroutine.close(1)", "coroutine.yield()"],
        "package": ["require()", "require({})", "package.searchpath()", "package.searchpath({}, '')"],
        "debug": ["debug.getinfo()", "debug.getlocal()", "debug.getupvalue()", "debug.setmetatable()", "debug.traceback({}, 1)"],
    }
    for lib, samples in arg_samples.items():
        for i, src in enumerate(samples):
            name = {("math", 3): "math.tointeger_invalid_ok",
                    ("debug", 4): "debug.traceback_object_ok"}.get((lib, i), f"{lib}.{i:02}")
            add("argument", name, src)
    for name, src in {
        "bad_method_self": "local s='x'; s:sub()",
        "renamed": "local w=string.rep; string.rep=nil; w({})",
        "field_renamed": "local t={w=string.rep}; string.rep=nil; t.w()",
        "lua_named": "local function foo() return string.rep() end; foo()",
    }.items():
        add("argument", name, src)

    metas = {
        "index_body": "local t=setmetatable({}, {__index=function() error('index body') end}); return t.x",
        "index_chain": "local t=setmetatable({}, {__index=setmetatable({}, {__index=false})}); return t.x",
        "index_noncall": "local t=setmetatable({}, {__index=3}); return t.x",
        "add_body": "local t=setmetatable({}, {__add=function() error('add body') end}); return t+1",
        "add_noncall": "local t=setmetatable({}, {__add=3}); return t+1",
        "len_bad_result": "local t=setmetatable({}, {__len=function() return {} end}); return #t + 1",
        "lt_truthy_result_ok": "local t=setmetatable({}, {__lt=function() return {} end}); return t < {}",
        "eq_truthy_result_ok": "local t=setmetatable({}, {__eq=function() return {} end}); return t == {}",
        "concat_body": "local t=setmetatable({}, {__concat=function() error('concat body') end}); return t .. 'x'",
        "call_missing": "local t={}; return t()",
        "call_noncall": "local t=setmetatable({}, {__call=3}); return t()",
        "close_body": "local mt={__close=function() error('close body') end}; do local x <close> = setmetatable({},mt) end",
        "tostring_result": "local t=setmetatable({}, {__tostring=function() return {} end}); return tostring(t)",
        "gc_warning_ok": "local t=setmetatable({}, {__gc=function() error('gc body') end}); t=nil; collectgarbage()",
        "wrap_string": "local f=coroutine.wrap(function() error('wrapped') end); f()",
        "wrap_nonstring": "local f=coroutine.wrap(function() error(42) end); f()",
        "wrap_dead": "local f=coroutine.wrap(function() end); f(); f()",
        "wrap_close": "local f=coroutine.wrap(function() local x <close> = setmetatable({}, {__close=function() error('close') end}) end); f()",
        "xpcall_transform": "local ok,e=xpcall(function() error('original') end,function() return 'transformed' end); error(e,0)",
        "xpcall_nonstring": "local ok,e=xpcall(function() error('original') end,function() return 42 end); error(e,0)",
    }
    for name, src in metas.items():
        add("metamethod", name, src)
    for name, src in {
        "runtime": "local f=assert(load('local x=nil; return x.y', '@inner.lua')); local g=assert(load(string.dump(f,true))); g()",
        "level": "local f=assert(load('error(\"bad\",1)', '@inner.lua')); local g=assert(load(string.dump(f,true))); g()",
        "argument": "local f=assert(load('return string.rep()', '@inner.lua')); local g=assert(load(string.dump(f,true))); g()",
        "traceback": "local f=assert(load('error(\"bad\")', '@inner.lua')); local g=assert(load(string.dump(f,true))); local ok,e=pcall(g); error(debug.traceback(e),0)",
    }.items():
        add("stripped", name, src)

    for name, src in {
        "index": "local u=newud(0); return u.x",
        "index_assign": "local u=newud(0); u.x=1",
        "call": "local u=newud(0); u()",
        "add": "local u=newud(0); return u+1",
        "bitwise": "local u=newud(0); return u&1",
        "length": "local u=newud(0); return #u",
        "concat": "local u=newud(0); return u..'x'",
        "compare": "local u=newud(0); return u<1",
        "light_index": "local u=light(1); return u.x",
        "error_object": "error(newud(0), 1)",
    }.items():
        add("userdata", name, src)

    compile_sources = {
        "unfinished_string": "local x = 'abc", "unfinished_long_string": "local x = [[abc",
        "unfinished_comment": "--[[ comment", "escape_x": "local x='\\xZZ'",
        "escape_u": "local x='\\u{XYZ}'", "escape_decimal": "local x='\\999'",
        "bad_long_bracket": "local x=[=abc", "malformed_number": "local x=0xGG",
        "unexpected": "local x = *", "expected_then": "if true print(1) end",
        "expected_do": "while true print(1) end", "expected_end": "if true then print(1)",
        "expected_paren": "print(1", "expected_brace": "local x={1,2",
        "expected_name": "local = 1", "expected_equal": "local x <const> 1",
        "bad_attribute": "local x <bad> = 1", "const_assign": "local x <const> = 1; x=2",
        "duplicate_label": "::L:: ::L::",
        "duplicate_label_multiline": "::A:: a=1\n::A::\n",
        "adjacent_labels_multiline": "::A::\n::A::\n\n",
        "unresolved_goto": "goto nowhere",
        "unresolved_goto_multiline": "a=1\ngoto A\ndo ::A:: end\n",
        "goto_scope": "goto L; local x=1; ::L:: print(x)",
        "goto_scope_multiline": "goto L\nlocal x=1\n::L:: print(x)",
        "break_outside": "break", "break_outside_multiline": "break\n",
        "upvalues_200_ok": (
            "local " + ",".join(f"a{i}" for i in range(1, 201)) + " = 1\n"
            "return function() return " + "+".join(f"a{i}" for i in range(1, 201)) + " end"
        ),
        "vararg_outside": "local function f() return ... end",
        "unclosed_function": "function f()\n  if true then\n    print(1)\n  end",
        "unclosed_paren": "local x = (\n 1 + 2",
        "eof": "local x =",
        "duplicate_attribute": "local x <const> <close> = 1",
        "too_many_locals": "local " + ",".join(f"x{i}" for i in range(201)) + " = 1",
        "too_many_upvalues": (
            "local " + ",".join(f"a{i}" for i in range(130)) + " = 1\n"
            "return function() local " + ",".join(f"b{i}" for i in range(130)) + " = 1\n"
            "return function() return " + "+".join(f"a{i}" for i in range(130)) + "+"
            + "+".join(f"b{i}" for i in range(130)) + " end end"
        ),
    }
    names = {"file": "@diag.lua", "equal": "=chunk", "literal": "source chunk", "default": None,
             "long": "@" + "very/long/path/" * 10 + "diag.lua"}
    for name, src in compile_sources.items():
        for label, chunk in names.items():
            add("compile", f"{name}.{label}", src, chunk=chunk)
    return out


def generated():
    out = []
    bad = {
        "nil": "nil", "boolean": "true", "number": "1", "string": "'bad'",
        "table": "{}", "function": "function() end",
        "thread": "coroutine.create(function() end)",
    }
    forms = {
        "local": ("local v={v}; ", "v"),
        "upvalue": ("local v={v}; local function f() ", "v"),
        "global": ("diag_v={v}; ", "diag_v"),
        "field": ("local t={{v={v}}}; ", "t.v"),
        "method_result": ("local t={{f=function() return {v} end}}; ", "t:f()"),
        "temporary": ("local function f() return {v} end; ", "f()"),
        "constant": ("", "({v})"),
    }
    ops = {
        "call": "{x}()", "index": "local z={x}.q", "index_assign": "{x}.q=1",
        "add": "local z={x}+1", "sub": "local z={x}-1", "mul": "local z={x}*1",
        "div": "local z={x}/1", "idiv": "local z={x}//1", "mod": "local z={x}%1",
        "pow": "local z={x}^1", "unm": "local z=-{x}",
        "band": "local z={x}&1", "bor": "local z={x}|1", "bxor": "local z={x}~1",
        "shl": "local z={x}<<1", "shr": "local z={x}>>1", "bnot": "local z=~{x}",
        "concat": "local z={x}..{{}}", "length": "local z=#{x}",
        "lt": "local z={x}<1", "le": "local z={x}<=1", "gt": "local z={x}>1", "ge": "local z={x}>=1",
    }
    for form, (prefix, ref) in forms.items():
        for typ, value in bad.items():
            for opname, op in ops.items():
                if (opname == "call" and typ == "function") or (opname.startswith("index") and typ == "table"):
                    continue
                if opname == "index" and typ == "string":
                    continue  # Lua's string metatable supplies __index.
                if opname in {"add", "sub", "mul", "div", "idiv", "mod", "pow", "unm"} and typ == "number":
                    continue
                if opname in {"band", "bor", "bxor", "shl", "shr", "bnot"} and typ == "number":
                    continue
                if opname == "concat" and typ in {"number", "string"}:
                    continue
                if opname == "length" and typ in {"table", "string"}:
                    continue
                if opname in {"lt", "le", "gt", "ge"} and typ == "number":
                    continue
                expression = ref.replace("{v}", value)
                src = prefix.format(v=value) + op.replace("{x}", expression).replace("{{}}", "{}")
                if form == "upvalue":
                    src += " end; f()"
                out.append(case("generated_runtime", f"{form}.{typ}.{opname}", src))
            # The bad operand is also tested on the right. PUC names the
            # offending operand, and some operators reverse their probes.
            right_ops = {
                "add": "1+{x}", "sub": "1-{x}", "mul": "1*{x}",
                "div": "1/{x}", "idiv": "1//{x}", "mod": "1%{x}",
                "pow": "1^{x}", "band": "1&{x}", "bor": "1|{x}",
                "bxor": "1~{x}", "shl": "1<<{x}", "shr": "1>>{x}",
                "concat": "{}..{x}", "lt": "1<{x}", "le": "1<={x}",
                "gt": "1>{x}", "ge": "1>={x}",
            }
            for opname, expression in right_ops.items():
                if typ == "number" and opname != "concat":
                    continue
                if opname == "concat" and typ in {"number", "string"}:
                    continue
                expression = expression.replace("{x}", ref.replace("{v}", value))
                src = prefix.format(v=value) + "local z=" + expression
                if form == "upvalue":
                    src += " end; f()"
                out.append(case("generated_runtime", f"{form}.{typ}.rhs_{opname}", src))
    # Explicit nested data flow beyond the regular source forms.
    nested = {
        "a_b_c_d": "local a={b={c={d={v}}}}; local z=a.b.c.d.q",
        "f_x_y": "local function f() return {x={y={v}}} end; local z=f().x.y.q",
        "or": "local a=nil; local b={v}; local z=(a or b).q",
        "t_k": "local t={}; local k=1; t[k]={v}; local z=t[k].q",
        "t_1": "local t={{v}}; local z=t[1].q",
    }
    for name, template in nested.items():
        for typ, value in bad.items():
            if typ in {"table", "string"}:
                continue
            out.append(case("generated_runtime", f"nested.{name}.{typ}", template.replace("{v}", value)))
    # A bad first argument is valid for every named function below. The other
    # variants exercise the library's own missing/range checks where present.
    functions = {
        "string.rep": ("{}", "", "'x', 1000000000000000000"),
        "string.sub": ("{}", "", None),
        "string.format": ("{}", "", None),
        "string.char": ("{}", None, "256"),
        "string.byte": ("{}", None, None),
        "table.insert": ("'x', 1", "", "{}, 0, 'x'"),
        "table.concat": ("'x'", "", "{}, ',', 0"),
        "table.unpack": ("true", "", None),
        "table.remove": ("'x'", "", "{}, 2"),
        "math.floor": ("{}", "", None),
        "math.max": ("{}, 1", "", None),
        "tonumber": ("{}, 2", None, "'10', 1"),
        "setmetatable": ("1, {}", "", None),
        "select": ("{}", "", "0, 1"),
        "rawlen": ("true", "", None),
        "rawset": ("1, 'x', 2", "", "{}, nil, 2"),
        "error": ("'boom', {}", None, None),
        "coroutine.resume": ("{}", "", None),
        "coroutine.wrap": ("{}", "", None),
        "coroutine.status": ("{}", "", None),
        "debug.getinfo": ("{}", "", None),
        "debug.getlocal": ("{}", "", None),
    }

    def call(form, fn, args):
        invocation = f"f({args})"
        if form == "global":
            return f"{fn}({args})"
        if form == "local":
            return f"local f={fn}; {invocation}"
        if form == "upvalue":
            return f"local f={fn}; local function w() return {invocation} end; w()"
        if form == "field":
            return f"local t={{f={fn}}}; t.f({args})"
        if form == "method":
            # Method syntax passes the receiver as argument one. A table
            # receiver is deliberately wrong for string/math/coroutine APIs.
            return f"local t={{f={fn}}}; t:f({args})"
        if form == "metamethod_index":
            return f"local t=setmetatable({{}}, {{__index=function(_,k) if k=='f' then return {fn} end end}}); t.f({args})"
        if form == "metamethod_call":
            return f"local t=setmetatable({{}}, {{__call={fn}}}); t({args})"
        if form == "passed_value":
            return f"local function apply(f) return {invocation} end; apply({fn})"
        if form == "pcall":
            return f"local ok,e=pcall({fn}, {args}); if not ok then error(e,0) end"
        if form == "xpcall":
            return f"local ok,e=xpcall({fn}, function(e) return e end, {args}); if not ok then error(e,0) end"
        if form == "tail":
            return f"local f={fn}; local function w(...) return f(...) end; w({args})"
        if form == "tail_two_levels":
            return f"local f={fn}; local function w(...) return f(...) end; local function outer(...) return w(...) end; outer({args})"
        if form == "stripped":
            return ("local w=assert(load('return function(...) return " + fn
                    + "(...) end', '@stripped.lua')); local f=assert(load(string.dump(w(),true))); f(" + args + ")")
        if form == "renamed":
            return f"local f={fn}; {fn}=nil; local ok,e=pcall(f,{args}); {fn}=f; if not ok then error(e,0) end"
        if form == "mutated":
            return f"local f={fn}; {fn}=function() end; local ok,e=pcall(f,{args}); {fn}=f; if not ok then error(e,0) end"
        raise ValueError(form)

    forms = ("global", "local", "upvalue", "field", "method", "metamethod_index",
             "metamethod_call", "passed_value", "pcall", "xpcall", "tail",
             "tail_two_levels", "stripped", "renamed", "mutated")
    for fn, (wrong, missing, out_of_range) in functions.items():
        for form in forms:
            if form == "method" and fn in {"ipairs", "rawlen"}:
                continue
            # __call prepends a table to the argument list. A wrong first
            # argument is still produced for most functions, but table APIs
            # need a different receiver and are covered by method syntax.
            if form == "metamethod_call" and (fn.startswith("table.") or fn in {"rawlen", "rawset"}):
                continue
            method_args = {
                "table.insert": "0, 'x'", "table.concat": "',', 0",
                "table.unpack": "'x'", "table.remove": "2", "rawset": "nil, 2",
            }
            args = method_args.get(fn, wrong) if form == "method" else wrong
            out.append(case("generated_argument", f"{fn}.{form}.wrong", call(form, fn, args)))
            if missing is not None and form in ("global", "local", "upvalue", "field", "passed_value", "tail", "stripped"):
                out.append(case("generated_argument", f"{fn}.{form}.missing", call(form, fn, missing)))
            if out_of_range is not None and form in ("global", "local", "pcall"):
                kind = "bad_key" if fn == "rawset" else "range"
                out.append(case("generated_argument", f"{fn}.{form}.{kind}", call(form, fn, out_of_range)))
    out.append(case("generated_argument", "ipairs.iterator.wrong",
                    "for _ in ipairs(1) do end"))
    out.append(case("generated_argument", "ipairs.iterator.missing",
                    "for _ in ipairs() do end"))
    out.append(case("generated_argument", "math.tointeger_invalid_ok",
                    "math.tointeger({})"))
    out.append(case("generated_argument", "stateful.gmatch_iterator.wrong",
                    "local f=string.gmatch('a','%'); f()"))
    out.append(case("generated_argument", "stateful.coroutine_wrap.wrong",
                    "local f=coroutine.wrap(function() return string.rep({}) end); f()"))
    return out


def provenance_flow():
    """Exercise last-writer scans across branches, jumps, loops, and calls."""
    out = []
    operations = {
        "index": ("local z=x.q", ("nil", "false", "1")),
        "assign": ("x.q=1", ("nil", "false", "1")),
        "call": ("x()", ("nil", "false", "1")),
        "arith": ("local z=x+1", ("nil", "true", "{}")),
        "bitwise": ("local z=x&1", ("nil", "true", "{}")),
        "concat": ("local z=x..{}", ("nil", "true", "{}")),
        "length": ("local z=#x", ("nil", "true", "1")),
        "compare": ("local z=x<1", ("nil", "true", "{}")),
        "unary_minus": ("local z=-x", ("nil", "true", "{}")),
        "unary_bitnot": ("local z=~x", ("nil", "true", "{}")),
        "two_temps": ("local y=(function() return 1 end)(); local z=x+y", ("nil", "true", "{}")),
        "chain_concat": ("local z='a'..x..'b'", ("nil", "true", "{}")),
    }
    # Each shape has the same failing operation, but a different route to x.
    # Values in the two branch arms are deliberately identical: selecting an
    # arm changes control flow without changing which operation fails.
    flows = {
        "and_true": "local t={a={v}}; local u={b={v}}; local c=true; local x=(c and t.a or u.b); {op}",
        "and_false": "local t={a={v}}; local u={b={v}}; local c=false; local x=(c and t.a or u.b); {op}",
        "if_else": "local x; if false then x=1 else x=(function() return {v} end)() end; {op}",
        "while_before": "local x={v}; local i=0; while i<2 do i=i+1; if i==2 then {op} end end",
        "while_inside": "local x=1; local i=0; while i<2 do i=i+1; x={v}; if i==2 then {op} end end",
        "repeat_inside": "local x=1; local i=0; repeat i=i+1; x={v}; if i==2 then {op} end until i==2",
        "numeric_for": "local x=1; for i=1,2 do x={v}; if i==2 then {op} end end",
        "generic_for": "local x=1; for i in ipairs({1,2}) do x={v}; if i==2 then {op} end end",
        "goto_over": "local x={v}; goto use; x=1; ::use:: {op}",
        "multiple": "local x,y={v},1; {op}",
        "call_result": "local function f() return {v} end; local x=f(); {op}",
        "vararg": "local function f(...) local x=...; {op} end; f({v})",
        "method_result": "local t={f=function() return {v} end}; local x=t:f(); {op}",
        "upvalue_shadow": "local x={v}; local function f() do local x=1 end; {op} end; f()",
        "const": "local x <const> = {v}; {op}",
        "scope_reuse": "do local x=1 end; local x=({v}); {op}",
        "long_distance": "local x={v}; " + " ".join(f"local pad{i}={i};" for i in range(72)) + " {op}",
    }
    for flow, template in flows.items():
        for opname, (op, values) in operations.items():
            for value_no, value in enumerate(values):
                out.append(case("provenance_flow", f"{flow}.{opname}.{value_no}",
                                template.replace("{v}", value).replace("{op}", op)))

    before_loops = {
        "repeat_before": "local x=nil; local i=0; repeat i=i+1; if i==2 then {op} end until i==2",
        "numeric_before": "local x=nil; for i=1,2 do if i==2 then {op} end end",
        "generic_before": "local x=nil; for i in ipairs({1,2}) do if i==2 then {op} end end",
    }
    for flow, template in before_loops.items():
        for opname, (op, _) in operations.items():
            out.append(case("provenance_flow", f"{flow}.{opname}", template.replace("{op}", op)))

    # Writers and operands whose spelling cannot be expressed by the regular
    # x template. Keep these separate so the exact source remains inspectable.
    extras = {
        "constructor.array": "local x=({nil, 1})[1]; local z=x.q",
        "constructor.record": "local x=({a=nil}).a; local z=x.q",
        "multiple.rhs": "local a,x=1,({}).missing; local z=x.q",
        "select.vararg": "local function f(...) local x=select(2,...); return x.q end; f(1,nil)",
        "vararg.direct": "local function f(...) return (...).q end; f(nil)",
        "vararg.multiple": "local function f(...) local a,b=...; return b.q end; f(1,nil)",
        "call.index": "local function f() return nil end; local z=f().q",
        "method.index": "local t={f=function() return nil end}; local z=t:f().q",
        "or.call": "local a=nil; local b=false; (a or b)()",
        "nested.shadow": "local x=nil; local function outer() local x=1; return function() return x.q end end; outer()()",
        "key.integer": "local t={}; local z=t[1].q",
        "key.float": "local t={}; local z=t[1.5].q",
        "key.string": "local t={}; local z=t['k'].q",
        "key.boolean": "local t={}; local z=t[true].q",
        "key.local": "local t={}; local k='k'; local z=t[k].q",
        "key.upvalue": "local t={}; local k='k'; local function f() return t[k].q end; f()",
        "key.global": "diag_key='k'; local t={}; local z=t[diag_key].q",
        "two_calls.arith": "local function f() return nil end; local function g() return 1 end; local z=f()+g()",
        "unary_not_ok": "local function f() return nil end; local z=not f()",
    }
    for name, src in extras.items():
        out.append(case("provenance_flow", name, src))
    return out


def stripped():
    """Run source forms inside dump(true)/load with line data removed."""
    out = []

    def add(name, body):
        # lua_quote preserves newlines and quotes in the source passed to load.
        src = ("local f=assert(load(" + lua_quote(body) + ", '@stripped.lua')); "
               "local g=assert(load(string.dump(f,true))); g()")
        out.append(case("stripped", name, src))

    forms = {
        "local": ("local x=nil; ", "x"),
        "upvalue": ("local x=nil; local function inner() ", "x"),
        "global": ("diag_stripped=nil; ", "diag_stripped"),
        "field": ("local t={x=nil}; ", "t.x"),
        "constant": ("", "(nil)"),
        "temporary": ("local function make() return nil end; ", "make()"),
    }
    ops = {
        "index": "local z={x}.q", "assign": "{x}.q=1", "call": "{x}()",
        "arith": "local z={x}+1", "bitwise": "local z={x}&1",
        "concat": "local z={x}..'b'", "length": "local z=#{x}",
        "compare": "local z={x}<1",
    }
    for form, (prefix, ref) in forms.items():
        for opname, op in ops.items():
            body = prefix + op.replace("{x}", ref)
            if form == "upvalue":
                body += " end; inner()"
            add(f"{form}.{opname}", body)

    for depth in ("direct", "nested"):
        for level in (1, 2):
            call = f"error('bad', {level})"
            body = call if depth == "direct" else f"local function inner() {call} end; inner()"
            add(f"error.{depth}.level{level}", body)

    arguments = {
        "direct": ("string.rep()", "string.sub()", "string.char({})",
                   "string.format('%d',{})"),
        "method": ("('x'):rep({})", "('x'):sub({})", "('x'):byte({})",
                   "('%d'):format({})"),
        "tail": ("local function w() return string.rep() end; w()",
                 "local function w() return string.sub() end; w()",
                 "local function w() return string.char({}) end; w()",
                 "local function w() return string.format('%d',{}) end; w()"),
    }
    for form, invocations in arguments.items():
        for variant, invocation in zip(("rep", "sub", "char_or_byte", "format"), invocations):
            add(f"argument.{form}.{variant}", invocation)

    for depth in range(8):
        pads = " ".join(f"local pad{i}={i};" for i in range(depth))
        body = (pads + " local ok,e=pcall(function() error('bad') end); "
                "local t=debug.traceback(e,1); "
                "error(t:match('^[^\\n]*\\n[^\\n]*') or t,0)")
        add(f"traceback.{depth:02}", body)

    for depth in range(8):
        pads = "\n" * depth
        body = pads + "local info=debug.getinfo(1,'Sl'); error(tostring(info.currentline)..'|'..info.short_src,0)"
        add(f"getinfo.{depth:02}", body)
    return out


def review_cases():
    sources = {
        "env-alias": "local env=_ENV; return env.missing+1",
        'float-left': 'local a=1.2; local b=2; return a&b',
        'float-both': 'local a=1.2; local b=2.2; return a&b',
        'assign': 'local a=3; local b={}; a.x,b.x=1,2',
        'assign-second': 'local a={}; local b=3; a.x,b.x=1,2',
        'env-copy': 'local _ENV={}; _ENV.x:y()',
        'env-up': 'local _ENV={}; local function f() return x+1 end; f()',
        'env-shadow': 'local _ENV={a={_ENV={}}}; return a._ENV.x+1',
        'arg-rename': 'local f=math.sin; math.sin=nil; math.zzz=f; local _,e=pcall(f,{}); math.sin=f; math.zzz=nil; error(e,0)',
        'arg-module': 'local f=math.sin; math.sin=nil; package.loaded.newmodule={renamed=f}; local _,e=pcall(f,{}); math.sin=f; package.loaded.newmodule=nil; error(e,0)',
        'chunknul': 'local f=load("local a=nil; return a.x", "@abc\\0def"); f()',
        'wrap-double': 'coroutine.wrap(function() error("x") end)()',
        'methodlocal': 'local a=3; a:x()',
        'methodup': 'local a=3; local function f() a:x() end; f()',
        'methodglobal': 'a=3; a:x()',
        'load-status': 'local s="return "..string.rep("(",200).."1"..string.rep(")",200); local ok,f,e=pcall(load,s); error(tostring(ok).."|"..type(f).."|"..tostring(e),0)',
        'method-field': 'local aaa={bbb=1}; aaa.bbb:ddd(9)',
        'env-local': 'local _ENV={}; a=a+1',
        'load-return': 'local s="return "..string.rep("(",200).."1"..string.rep(")",200); local f,e=load(s); error(type(f).."|"..tostring(e),0)',
    }
    compile_sources = {
        'token-nul': 'return \x00',
        'token-control': 'return \x01',
        'token-del': 'return \x7f',
        'token-escape': 'local "a\\n\\x42"',
        'token-binary-string': 'local "a\x00b"',
        'token-newline': 'local x="abc\ndef"',
        'token-bad-escape': 'local x="a\\n\\q"',
        'token-long-newline': 'local [[\nabc]]',
        'token-long-cr': 'local [=[a\rb]=]',
    }
    for name, source in compile_sources.items():
        sources[name] = "local f,e=load(" + lua_quote(source) + ", '@diag.lua'); error(e,0)"
    return [case("review", name, source) for name, source in sources.items()]


def flat_cases():
    sources = {
        "add.ok": "return 1" + "+1" * 1000,
        "logical.ok": "return true" + " and true" * 1000,
        "field.ok": "local t={}; t.x=t; return t" + ".x" * 1000,
        "call.ok": "local function f() return f end; return f" + "()" * 1000,
        "method.ok": "local o={}; function o:m() return self end; return o" + ":m()" * 1000,
        "mixed.ok": "local t={}; local function f() return t end; t.x=f; return t" + ".x()" * 500,
        "arithmetic-line": "local n=1\nreturn n" + "+1" * 1000 + "\n + true",
        "suffix-line": "local t={}; t.x=t\nreturn t" + ".x" * 1000 + "\n .missing.x",
    }
    return [case("flat", name, source) for name, source in sources.items()]


def all_cases():
    cases = handwritten() + generated() + provenance_flow() + stripped() + review_cases() + flat_cases()
    ids = [c["id"] for c in cases]
    if len(ids) != len(set(ids)):
        raise ValueError("duplicate diagnostic case id")
    return cases


def lua_quote(value):
    if value is None:
        return "nil"
    # Decimal byte escapes avoid host locale, Lua quoting, and UTF-8 ambiguity.
    return '"' + "".join(f"\\{b:03d}" for b in value.encode("utf-8")) + '"'


DRIVER = r'''
local function hex(s)
  local out = {}
  for i = 1, #s do out[i] = string.format('%%02x', string.byte(s, i)) end
  return table.concat(out)
end
local function execute(c)
  _diag_marker = nil
  if c[4] ~= '' then
    local setup, e = load(c[4], '@diag-setup.lua')
    if not setup then error('bad setup: ' .. e) end
    local ok, reason = pcall(setup)
    if not ok then error('failed setup: ' .. tostring(reason)) end
  end
  local f, cerr
  if c[3] == false then f, cerr = load(c[2]) else f, cerr = load(c[2], c[3]) end
  local state, typ, bytes, identity
  if not f then
    state, typ, bytes, identity = 'compile', 'string', hex(cerr), '-'
  else
    local ok, err = pcall(f)
    if ok then
      state, typ, bytes, identity = 'ok', '-', '-', '-'
    else
      state, typ, bytes, identity = 'error', type(err), '-', '-'
      if typ == 'string' then bytes = hex(err) end
      if typ == 'table' and _diag_marker ~= nil then
        identity = (err == _diag_marker) and 'same' or 'other'
      end
    end
  end
  print(c[1] .. '\t' .. state .. '\t' .. typ .. '\t' .. bytes .. '\t' .. identity)
end
local cases = {%s}
for _, c in ipairs(cases) do execute(c) end
'''


def driver(cases):
    rows = []
    for c in cases:
        chunk = "false" if c["chunk"] is None else lua_quote(c["chunk"])
        rows.append("{" + ",".join((lua_quote(c["id"]), lua_quote(c["source"]), chunk, lua_quote(c["setup"]))) + "}")
    return (DRIVER % ",".join(rows)).encode("ascii")


def grouped_batches(size=120):
    grouped = {}
    for item in all_cases():
        grouped.setdefault(item["group"], []).append(item)
    for group, cases in grouped.items():
        for offset in range(0, len(cases), size):
            yield f"{group}-{offset // size:03}", cases[offset : offset + size]


if __name__ == "__main__":
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=Path)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    for name, cases in grouped_batches():
        batch = args.out / name
        batch.mkdir(parents=True, exist_ok=True)
        (batch / "driver.lua").write_bytes(driver(cases))
    (args.out / "manifest.json").write_text(json.dumps(all_cases(), indent=2) + "\n")
