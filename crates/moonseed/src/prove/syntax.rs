//! `and`, `or`, `not`, method calls, call shorthands, function statements,
//! and `local function` (Phase 3.18): compiler lowering onto existing
//! instructions. The source fixtures that match Lua 5.4.9 are `logic_ops`
//! and `methods` in `NATIVE_FIXTURES`, and `methods_deep` in
//! `DEEP_FIXTURES`.

use super::generic_for::drive;
use super::*;
use crate::host::{HostValue, LegacyCompletion};
use crate::opcode::{Capture, Op};

fn run_source(source: &[u8]) -> Runtime {
    finish_with(boot_natives, &crate::compile(source).unwrap().proto)
}

/// Lua 5.4's rule: `v:name(args)` is `v.name(v, args)` with `v` evaluated
/// once.
#[test]
fn a_method_receiver_is_evaluated_once() {
    let source = b"local count = 0 \
        local make = function() \
            count = count + 1 \
            return { value = 42, get = function(self) return self.value end } \
        end \
        local result = make():get() \
        return count, result";
    assert_eq!(print_line(&run_source(source)), "1\t42\n");
    let spec = crate::compile(source).unwrap().proto;
    quantum_and_checkpoints_with(boot_natives, &spec, pair_results);
    // The receiver, then one `GetField` of `get` from it, then the call.
    let ops = &spec.ops;
    let call = ops
        .iter()
        .position(|op| matches!(op, Op::Call { nargs: 1, .. }))
        .unwrap();
    assert!(matches!(ops[call - 1], Op::GetField { .. }));
}

/// Long chains reuse their registers: 150 method calls in a chain, 150
/// operands of `or` and of `and`, a 50-deep function-statement name, and
/// 50 nested local functions each stay within a few registers.
#[test]
fn chains_of_the_new_forms_reuse_registers() {
    let chain = ":inc()".repeat(150);
    let ors = vec!["nil"; 149].join(" or ");
    let ands = vec!["1"; 149].join(" and ");
    let path = (0..50).map(|i| format!("k{i}")).collect::<Vec<_>>();
    let mut nested = String::new();
    for i in 0..50 {
        nested.push_str(&format!("local function f{i}() "));
    }
    nested.push_str("return 1 ");
    for i in (0..50).rev() {
        nested.push_str(&format!("end return f{i}() "));
    }
    let source = format!(
        "local c = {{ n = 0 }} function c:inc() self.n = self.n + 1 return self end          local t = {{}} local p = t {make}          function t.{path}() return 5 end          local x = {ors} or 7 local y = {ands} and 8          local function z() {nested} end          return c{chain}.n, x, y, t.{path}(), z()",
        make = path[..49]
            .iter()
            .map(|k| format!("p.{k} = {{}} p = p.{k}"))
            .collect::<Vec<_>>()
            .join(" "),
        path = path.join("."),
    );
    let chunk = crate::compile(source.as_bytes()).unwrap();
    fn widest(spec: &crate::program::ProtoSpec) -> u8 {
        spec.children.iter().map(widest).fold(spec.max_reg, u8::max)
    }
    assert!(widest(&chunk.proto) <= 12, "{}", widest(&chunk.proto));
    let runtime = finish_with(boot_natives, &chunk.proto);
    assert_eq!(print_line(&runtime), "150\t7\t8\t5\t1\n");
}

/// A wait in the receiver's expression, in the method lookup (a Lua
/// `__index` that waits), and in the final `__newindex` store of a function
/// statement: each restored at the wait and after the completion, with the
/// receiver made once and the same fuel.
#[test]
fn waits_inside_method_calls_and_function_statements_restore() {
    let back = |values: Vec<HostValue>| LegacyCompletion::Return(values);
    let native = |name: &str| HostValue::Native(name.to_string());
    let cases: Vec<(&[u8], Vec<LegacyCompletion>, &str)> = vec![
        (
            b"local n = 0 \
              local make = function() n = n + 1 \
                return setmetatable({}, { __index = function(t, k) return park() end }) end \
              local r = make():m(5) return n, r",
            vec![back(vec![native("second")])],
            "1\t5\n",
        ),
        (
            b"local n = 0 local o = { get = function(self, x) return self.k + x end, k = 1 } \
              local make = function() n = n + 1 park() return o end \
              return make():get(2), n",
            vec![back(vec![])],
            "3\t1\n",
        ),
        (
            b"local t = setmetatable({}, { __newindex = function(t, k, v) park() rawset(t, k, v) end }) \
              function t.f() return 3 end function t:g() return self == t end \
              return t.f(), t:g()",
            vec![back(vec![]), back(vec![])],
            "3\ttrue\n",
        ),
    ];
    for (source, answers, line) in cases {
        let (straight, fuel) = drive(source, &answers, false);
        assert_eq!(straight, line, "{}", String::from_utf8_lossy(source));
        assert_eq!(drive(source, &answers, true), (straight, fuel));
    }
}

/// The right operand runs only when the left does not decide, including
/// when the left waits and is restored; nothing evaluates twice.
#[test]
fn logical_operators_short_circuit_across_waits() {
    let back = |values: Vec<HostValue>| LegacyCompletion::Return(values);
    let int = HostValue::Integer;
    let source = b"local n = 0 local bump = function() n = n + 1 return 9 end \
        local a = park() or bump() local b = park() and bump() \
        return a, b, n";
    for (answers, line) in [
        (
            vec![back(vec![int(7)]), back(vec![HostValue::Nil])],
            "7\tnil\t0\n",
        ),
        (
            vec![back(vec![HostValue::Boolean(false)]), back(vec![int(1)])],
            "9\t9\t2\n",
        ),
        (
            vec![
                back(vec![HostValue::Nil]),
                back(vec![HostValue::Boolean(false)]),
            ],
            "9\tfalse\t1\n",
        ),
    ] {
        let (straight, fuel) = drive(source, &answers, false);
        assert_eq!(straight, line);
        assert_eq!(drive(source, &answers, true), (straight, fuel));
    }
    // Both sides wait.
    let (line, _) = drive(
        b"return park() and park()",
        &[back(vec![int(1)]), back(vec![int(2), int(3)])],
        true,
    );
    assert_eq!(line, "2\n");
}

/// The lowering adds no opcode: `and` and `or` are a register, a
/// conditional jump or two, and the right operand; `not` is a jump and two
/// `LoadBool`s. A skipped right operand costs no fuel, however long.
#[test]
fn logical_operators_lower_to_jumps() {
    let ops = |source: &str| crate::compile(source.as_bytes()).unwrap().proto.ops;
    for op in
        ops("local a, b = ... local x = a and b local y = a or b local z = not a return x, y, z")
    {
        assert!(
            matches!(
                op,
                Op::Vararg { .. }
                    | Op::Move { .. }
                    | Op::JumpIfFalse { .. }
                    | Op::Jump { .. }
                    | Op::LoadBool { .. }
                    | Op::Return { .. }
            ),
            "{op:?}"
        );
    }
    let fuel = |source: &str| run_source(source.as_bytes()).fuel_consumed();
    assert_eq!(
        fuel("local x = false and 1 return x"),
        fuel("local x = false and add(add(1, 2), add(3, 4)) return x")
    );
    assert_eq!(
        fuel("local x = 1 or 2 return x"),
        fuel("local x = 1 or add(add(1, 2), add(3, 4)) return x")
    );
    // No metamethod: `__eq`, `__index`, `__call` are not consulted.
    assert_eq!(
        print_line(&run_source(
            b"local t = setmetatable({}, { __call = error, __index = error, __len = error }) \
              return not t, t and 1, nil or t == t"
        )),
        "false\t1\ttrue\n"
    );
}

/// `local function f` puts `f` in scope before its body, so the body
/// captures the new local; `local f = function` does not.
#[test]
fn a_local_function_captures_its_own_local() {
    let chunk = crate::compile(b"local x local function f() return f end return f").unwrap();
    // `f` is register 1, after `x`.
    assert_eq!(chunk.proto.children[0].captures, vec![Capture::Local(1)]);
    let chunk = crate::compile(b"local x local f = function() return f end return f").unwrap();
    assert!(
        !chunk.proto.children[0]
            .captures
            .contains(&Capture::Local(1))
    );
    // The closure is its own upvalue's value, and shadowing is lexical.
    assert_eq!(
        print_line(&run_source(
            b"local f = 1 local g do local function f() return f end g = f end \
              return g() == g, f"
        )),
        "true\t1\n"
    );
}

/// `return` of a method call, and of a `local function`'s call to itself,
/// is a tail call like any other; inside a `<close>` scope it is not.
#[test]
fn new_call_forms_keep_tail_call_eligibility() {
    let tail_calls = |ops: &[Op]| {
        ops.iter()
            .filter(|op| matches!(op, Op::TailCall { .. }))
            .count()
    };
    let chunk = crate::compile(
        b"local o = {} function o:f(n) if n == 0 then return self end return self:f(n - 1) end \
          local function loop(n) if n == 0 then return 0 end return loop(n - 1) end \
          function o:c(n) local x <close> = nil return self:f(n) end \
          return 0",
    )
    .unwrap();
    let children = &chunk.proto.children;
    assert_eq!(tail_calls(&children[0].ops), 1);
    assert_eq!(tail_calls(&children[1].ops), 1);
    assert_eq!(tail_calls(&children[2].ops), 0);
}

/// Lua does not end a statement at a newline: a `(` on the next line
/// continues the expression as a call, as in Lua 5.4.
#[test]
fn a_parenthesis_on_the_next_line_continues_the_expression() {
    let chunk = crate::parse::parse(b"local a = b + c\n(print or f)('done')").unwrap();
    assert_eq!(chunk.body.stmts.len(), 1);
    let chunk = crate::parse::parse(b"local a = b + c;\n(print or f)('done')").unwrap();
    assert_eq!(chunk.body.stmts.len(), 3);
}

/// Method names must be followed by arguments; function-statement names
/// take one `:` part, last.
#[test]
fn malformed_method_and_function_names_are_syntax_errors() {
    for source in [
        "local x = o:m",
        "o:m.x()",
        "function o:m.x() end",
        "function o:m:n() end",
        "function () end",
        "local function o.m() end",
        "function f",
    ] {
        match crate::compile(source.as_bytes()) {
            Err(error) => assert_eq!(error.kind, crate::CompileErrorKind::Syntax, "{source}"),
            Ok(_) => panic!("compiled: {source}"),
        }
    }
}

/// `<const>` locals cannot be assigned after their declaration, directly,
/// in a list, from a nested function at any depth, or by a function
/// statement; a name takes one attribute; attributes belong to `local`
/// only; and a goto cannot enter a `<const>` local's scope. All as Lua
/// 5.4.9 refuses them. A literal `<const>` keeps the same executable code
/// but, as in PUC, has no LocVar diagnostic name.
#[test]
fn const_locals_are_read_only_and_literal_names_are_omitted() {
    for source in [
        "local x <const> = 1 x = 2",
        "local x <const> = 1 x, y = 2, 3",
        "local x <const> = 1 y, x = 2, 3",
        "local x <const> = 1 local f = function() x = 2 end",
        "local x <const> = 1 local f = function() return function() x = 2 end end",
        "local x <const> = 1 function x() end",
        "local x <const> <close> = nil",
        "local x <foo> = 1",
        "local f = function(x <const>) end",
        "for i <const> = 1, 2 do end",
        "goto L local x <const> = 1 ::L:: return x",
    ] {
        match crate::compile(source.as_bytes()) {
            Err(error) => assert_eq!(error.kind, crate::CompileErrorKind::Syntax, "{source}"),
            Ok(_) => panic!("compiled: {source}"),
        }
    }
    let plain = crate::compile(b"local x = 42 local f = function() return x end return f()")
        .unwrap()
        .proto;
    let constant =
        crate::compile(b"local x <const> = 42 local f = function() return x end return f()")
            .unwrap()
            .proto;
    assert_eq!(plain.ops, constant.ops);
    assert_eq!(plain.children[0].ops, constant.children[0].ops);
    assert!(
        plain
            .debug
            .as_ref()
            .unwrap()
            .locals
            .iter()
            .any(|local| local.name == b"x")
    );
    assert!(
        !constant
            .debug
            .as_ref()
            .unwrap()
            .locals
            .iter()
            .any(|local| local.name == b"x")
    );
}
