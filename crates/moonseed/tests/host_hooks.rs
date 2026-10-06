//! Public API embedding witnesses: no internal handles or runtime helpers.
use moonseed::{
    ApiError, Error, HookAction, HookEvent, HookMask, Host, HostRegistry, Journal, Libraries,
    NativePolicy, Runtime, SnapshotError, StepOutcome, Value, compile,
};
use std::cell::RefCell;
use std::rc::Rc;

fn finish(runtime: &mut Runtime, checkpoint: bool, registry: &HostRegistry) {
    let mut journal = Journal::new();
    for _ in 0..50_000 {
        if checkpoint {
            *runtime = Runtime::restore(&runtime.snapshot().unwrap(), &Host::new(registry.clone()))
                .unwrap();
        }
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => return,
            other => panic!("unexpected {other:?}"),
        }
    }
    panic!("did not finish");
}
#[test]
fn profiler_records_lua_call_return_and_tail() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let output = events.clone();
    let mut registry = HostRegistry::new();
    registry.register_hook("profile", move |cx| {
        let info = cx.info(0)?.unwrap();
        if info.what == "Lua" {
            output
                .borrow_mut()
                .push((cx.event(), info.istailcall, info.ntransfer));
        }
        Ok(HookAction::Continue)
    });
    let mut runtime = Runtime::builder()
        .registry(registry)
        .libraries(Libraries::ALL)
        .build()
        .unwrap();
    let chunk = compile(b"local function f(a) return a,nil end local function g(a) return f(a) end local a,b=g(7) return a,b").unwrap();
    runtime.load_main(&chunk).unwrap();
    runtime
        .set_hook(None, "profile", HookMask::CALL | HookMask::RETURN, 0)
        .unwrap();
    finish(&mut runtime, false, &HostRegistry::new());
    assert_eq!(
        &*events.borrow(),
        &[
            (HookEvent::Call, false, 1),
            (HookEvent::TailCall, true, 1),
            (HookEvent::Return, true, 2)
        ]
    );
    assert!(matches!(
        runtime.result_values().unwrap().as_slice(),
        [Value::Integer(7), Value::Nil]
    ));
}
#[test]
fn line_debugger_reads_upvalues_and_edits_a_local() {
    let observed = Rc::new(RefCell::new(false));
    let seen = observed.clone();
    let mut registry = HostRegistry::new();
    registry.register_hook("debugger", move |cx| {
        let info = cx.info(0)?.unwrap();
        if info.what == "Lua" && cx.line() == Some(4) {
            let (name, local) = cx.local(0, 1)?.unwrap();
            assert_eq!(name, b"x");
            assert_eq!(local.as_integer(), Some(3));
            let (name, upvalue) = cx.upvalue(0, 1)?.unwrap();
            assert_eq!(name, b"secret");
            assert_eq!(upvalue.as_integer(), Some(7));
            assert_eq!(cx.set_local(0, 1, 10)?, Some(b"x".to_vec()));
            *seen.borrow_mut() = true;
        }
        Ok(HookAction::Continue)
    });
    let mut runtime = Runtime::builder().registry(registry).build().unwrap();
    runtime.load_main(&compile(b"local secret=7\nlocal function f()\n local x=3\n return x+secret\nend\nreturn f()").unwrap()).unwrap();
    runtime
        .set_hook(None, "debugger", HookMask::LINE, 0)
        .unwrap();
    finish(&mut runtime, false, &HostRegistry::new());
    assert!(*observed.borrow());
    assert!(matches!(
        runtime.result_values().unwrap().as_slice(),
        [Value::Integer(17)]
    ));
}
fn preempt_registry() -> HostRegistry {
    let mut registry = HostRegistry::new();
    moonseed::register_standard(&mut registry);
    moonseed::register_debug(&mut registry);
    registry.register_hook("preempt", |_| Ok(HookAction::Yield));
    registry.typed("install", NativePolicy::VmLocal, |cx, ()| {
        cx.set_hook(None, "preempt", HookMask::NONE, 1)
    });
    registry
}
#[test]
fn count_yield_restores_exact_continuation_with_nil_holes() {
    let registry = preempt_registry();
    let source = b"local co=coroutine.create(function() install() local a=0 for i=1,4 do a=a+i end local t={a,nil,7} return table.unpack(t,1,3) end) local r repeat r=table.pack(coroutine.resume(co,99)) until coroutine.status(co)=='dead' assert(r[1]) return table.unpack(r,2,r.n)";
    let mut fuel = None;
    for checkpoint in [false, true] {
        let mut runtime = Runtime::builder()
            .registry(registry.clone())
            .libraries(Libraries::ALL)
            .build()
            .unwrap();
        let install = runtime.make_closure("install", ()).unwrap();
        let globals = runtime.globals();
        globals.raw_set(&mut runtime, "install", install).unwrap();
        runtime.load_main(&compile(source).unwrap()).unwrap();
        finish(&mut runtime, checkpoint, &registry);
        assert!(matches!(
            runtime.result_values().unwrap().as_slice(),
            [Value::Integer(10), Value::Nil, Value::Integer(7)]
        ));
        if let Some(fuel) = fuel {
            assert_eq!(runtime.fuel_consumed(), fuel);
        } else {
            fuel = Some(runtime.fuel_consumed());
        }
    }
}
#[test]
fn host_hook_restore_uses_symbols_and_rejects_missing_registration() {
    let mut registry = HostRegistry::new();
    registry.register_hook("hook", |_| Ok(HookAction::Continue));
    let mut runtime = Runtime::builder()
        .registry(registry.clone())
        .build()
        .unwrap();
    runtime
        .load_main(&compile(b"local a=0 for i=1,4 do a=a+i end return a").unwrap())
        .unwrap();
    runtime.set_hook(None, "hook", HookMask::LINE, 3).unwrap();
    runtime.run(1, &mut Journal::new()).unwrap();
    let bytes = runtime.snapshot().unwrap();
    assert!(matches!(
        Runtime::restore(&bytes, &Host::default()),
        Err(Error::Vm(moonseed::VmError::Snapshot(
            SnapshotError::UnknownHostSymbol
        )))
    ));
    let mut fresh = HostRegistry::new();
    fresh.register_hook("unrelated", |_| Ok(HookAction::Continue));
    fresh.register_hook("hook", |_| Ok(HookAction::Continue));
    let mut restored = Runtime::restore(&bytes, &Host::new(fresh.clone())).unwrap();
    assert_eq!(restored.snapshot().unwrap(), bytes);
    finish(&mut restored, true, &fresh);
    assert!(matches!(
        restored.result_values().unwrap().as_slice(),
        [Value::Integer(10)]
    ));
}
#[test]
fn thread_ownership_is_checked_before_mutation() {
    let registry = preempt_registry();
    let mut first = Runtime::builder()
        .registry(registry.clone())
        .libraries(Libraries::ALL)
        .build()
        .unwrap();
    first
        .load_main(&compile(b"return coroutine.create(function() end)").unwrap())
        .unwrap();
    finish(&mut first, false, &registry);
    let thread = first.result_values().unwrap().remove(0);
    let mut second = Runtime::builder().registry(registry).build().unwrap();
    let before = second.snapshot().unwrap();
    assert!(matches!(
        second.set_hook(Some(&thread), "preempt", HookMask::LINE, 0),
        Err(Error::Api(ApiError::WrongRuntime))
    ));
    assert!(matches!(
        second.get_hook(Some(&thread)),
        Err(Error::Api(ApiError::WrongRuntime))
    ));
    assert!(matches!(
        second.clear_hook(Some(&thread)),
        Err(Error::Api(ApiError::WrongRuntime))
    ));
    assert_eq!(second.snapshot().unwrap(), before);
}

#[test]
fn clearing_a_suspended_preemption_preserves_resume_and_close() {
    let mut registry = preempt_registry();
    registry.typed("clear", NativePolicy::VmLocal, |cx, (thread,): (Value,)| {
        cx.clear_hook(Some(&thread))
    });
    for closing in [false, true] {
        let mut runtime = Runtime::builder()
            .registry(registry.clone())
            .libraries(Libraries::ALL)
            .build()
            .unwrap();
        for name in ["install", "clear"] {
            let function = runtime.make_closure(name, ()).unwrap();
            runtime
                .globals()
                .raw_set(&mut runtime, name, function)
                .unwrap();
        }
        let action = if closing {
            "assert(coroutine.close(co)) return coroutine.status(co)"
        } else {
            "local ok,a,b=coroutine.resume(co,999) assert(ok) return a,b"
        };
        let source = format!(
            "local co=coroutine.create(function() install() return 7,nil end) assert(coroutine.resume(co)) clear(co) {action}"
        );
        runtime
            .load_main(&compile(source.as_bytes()).unwrap())
            .unwrap();
        finish(&mut runtime, true, &registry);
        let values = runtime.result_values().unwrap();
        if closing {
            let Value::String(status) = &values[0] else {
                panic!("missing status")
            };
            assert_eq!(status.as_bytes(&runtime).unwrap(), b"dead");
        } else {
            assert!(matches!(values.as_slice(), [Value::Integer(7), Value::Nil]));
        }
    }
}

#[test]
fn illegal_host_hook_yields_remain_catchable_under_checkpoints() {
    for event in [HookEvent::Call, HookEvent::Return, HookEvent::TailCall] {
        let mut registry = preempt_registry();
        registry.register_hook("illegal", move |cx| {
            Ok(
                if cx.event() == event && cx.info(0)?.is_some_and(|info| info.what == "Lua") {
                    HookAction::Yield
                } else {
                    HookAction::Continue
                },
            )
        });
        registry.typed("install_bad", NativePolicy::VmLocal, |cx, ()| {
            cx.set_hook(None, "illegal", HookMask::CALL | HookMask::RETURN, 0)
        });
        let mut runtime = Runtime::builder()
            .registry(registry.clone())
            .libraries(Libraries::ALL)
            .build()
            .unwrap();
        let function = runtime.make_closure("install_bad", ()).unwrap();
        runtime
            .globals()
            .raw_set(&mut runtime, "install_bad", function)
            .unwrap();
        runtime.load_main(&compile(b"local function f() return 7 end local function g() return f() end local ok,e=xpcall(function() install_bad() g() end,function(e) f() return e end) debug.sethook() assert(not ok and string.find(e,'attempt to yield across a C%-call boundary')) return ok").unwrap()).unwrap();
        finish(&mut runtime, true, &registry);
        assert!(matches!(
            runtime.result_values().unwrap().as_slice(),
            [Value::Boolean(false)]
        ));
    }
}

#[test]
fn enabling_lines_from_count_waits_for_the_next_instruction() {
    let lines = Rc::new(RefCell::new(Vec::new()));
    let observed = lines.clone();
    let mut registry = HostRegistry::new();
    registry.register_hook("next.line", move |cx| {
        observed.borrow_mut().push(cx.line());
        Ok(HookAction::Continue)
    });
    registry.register_hook("switch", |cx| {
        assert_eq!(cx.event(), HookEvent::Count);
        cx.set_hook("next.line", HookMask::LINE, 0)?;
        Ok(HookAction::Continue)
    });
    let mut runtime = Runtime::builder()
        .registry(registry.clone())
        .build()
        .unwrap();
    runtime
        .load_main(&compile(b"local x=0\nx=1\nreturn x").unwrap())
        .unwrap();
    runtime.set_hook(None, "switch", HookMask::NONE, 1).unwrap();
    finish(&mut runtime, true, &registry);
    assert_eq!(*lines.borrow(), vec![Some(2), Some(3)]);
}

#[test]
fn installed_hook_survives_loading_and_reusing_main() {
    let mut registry = HostRegistry::new();
    registry.register_hook("persistent", |_| Ok(HookAction::Continue));
    let mut runtime = Runtime::builder()
        .registry(registry.clone())
        .build()
        .unwrap();
    runtime
        .set_hook(None, "persistent", HookMask::LINE, 2)
        .unwrap();
    for _ in 0..3 {
        runtime
            .load_main(&compile(b"local x=1\nreturn x+2").unwrap())
            .unwrap();
        finish(&mut runtime, true, &registry);
        assert!(runtime.get_hook(None).unwrap().is_some());
        assert!(matches!(
            runtime.result_values().unwrap().as_slice(),
            [Value::Integer(3)]
        ));
    }
}

#[test]
fn hook_conversion_cannot_reenter_and_owned_values_remain_rooted() {
    struct Inspect;
    impl moonseed::IntoLua for Inspect {
        fn into_lua(self, runtime: &mut Runtime) -> moonseed::Result<Value> {
            assert!(matches!(
                runtime.run(1, &mut Journal::new()),
                Err(moonseed::VmError::Api(ApiError::InvalidCallState))
            ));
            assert!(runtime.snapshot().is_err());
            Ok(Value::Nil)
        }
    }
    let retained = Rc::new(RefCell::new(None));
    let output = retained.clone();
    let mut registry = HostRegistry::new();
    registry.register_hook("retain", move |cx| {
        let table = cx.create_table()?;
        cx.raw_set(&table, "value", Inspect)?;
        cx.raw_set(&table, "answer", 42)?;
        *output.borrow_mut() = Some(table);
        cx.clear_hook()?;
        Ok(HookAction::Continue)
    });
    let mut runtime = Runtime::builder().registry(registry).build().unwrap();
    runtime.load_main(&compile(b"return 1").unwrap()).unwrap();
    runtime.set_hook(None, "retain", HookMask::LINE, 0).unwrap();
    finish(&mut runtime, false, &HostRegistry::new());
    runtime.collect();
    let table = retained.borrow_mut().take().unwrap();
    assert_eq!(table.raw_get::<_, i64>(&mut runtime, "answer").unwrap(), 42);
}

#[test]
fn panicking_host_hook_leaves_runtime_unusable() {
    let mut registry = HostRegistry::new();
    registry.register_hook("panic", |_| panic!("host hook panic"));
    let mut runtime = Runtime::builder().registry(registry).build().unwrap();
    runtime.load_main(&compile(b"return 1").unwrap()).unwrap();
    runtime.set_hook(None, "panic", HookMask::LINE, 0).unwrap();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || runtime.run(2, &mut Journal::new())
        ))
        .is_err()
    );
    assert!(matches!(
        runtime.run(1, &mut Journal::new()),
        Err(moonseed::VmError::Api(ApiError::InvalidCallState))
    ));
    assert!(runtime.snapshot().is_err());
}

#[test]
fn panicking_sink_or_resolver_leaves_runtime_unusable() {
    use moonseed::{HostCapabilities, Libraries, ModuleResolver, Resolved, ResolverPolicy};
    struct Panics;
    impl ModuleResolver for Panics {
        fn resolve(&self, _: &[u8]) -> Resolved {
            panic!("resolver panic")
        }
    }
    let profiles = [
        (
            HostCapabilities::sandbox().output(|_| panic!("output panic")),
            &b"print('once')"[..],
        ),
        (
            HostCapabilities::sandbox().warnings(|_, _| panic!("warning panic")),
            b"warn('once')",
        ),
        (
            HostCapabilities::sandbox().entropy(|| panic!("entropy panic")),
            b"math.randomseed()",
        ),
        (
            HostCapabilities::sandbox().module_resolver(ResolverPolicy::External, Panics),
            b"pcall(require, 'm')",
        ),
    ];
    for (capabilities, source) in profiles {
        let mut runtime = Runtime::builder()
            .libraries(Libraries::STANDARD)
            .capabilities(capabilities)
            .build()
            .unwrap();
        runtime.load_main(&compile(source).unwrap()).unwrap();
        let mut journal = Journal::new();
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || runtime.run(1_000, &mut journal)
            ))
            .is_err()
        );
        // Before the fix a retried print wrote its text a second time.
        assert!(matches!(
            runtime.run(1_000, &mut journal),
            Err(moonseed::VmError::Api(ApiError::InvalidCallState))
        ));
        assert!(runtime.snapshot().is_err());
    }
}

#[test]
fn hook_conversion_cannot_replace_idle_main_while_child_runs() {
    struct Reload;
    impl moonseed::IntoLua for Reload {
        fn into_lua(self, runtime: &mut Runtime) -> moonseed::Result<Value> {
            assert!(matches!(
                runtime.load_main(&compile(b"return 99").unwrap()),
                Err(Error::Api(ApiError::InvalidCallState))
            ));
            Ok(Value::Nil)
        }
    }
    let observed = Rc::new(RefCell::new(false));
    let output = observed.clone();
    let mut registry = HostRegistry::new();
    registry.register_hook("pause", |_| Ok(HookAction::Yield));
    registry.typed(
        "install.pause",
        NativePolicy::VmLocal,
        |cx, (thread,): (Value,)| cx.set_hook(Some(&thread), "pause", HookMask::LINE, 0),
    );
    registry.register_hook("reload", move |cx| {
        let table = cx.create_table()?;
        cx.raw_set(&table, "value", Reload)?;
        *output.borrow_mut() = true;
        cx.clear_hook()?;
        Ok(HookAction::Continue)
    });
    let mut runtime = Runtime::builder()
        .registry(registry)
        .libraries(Libraries::ALL)
        .build()
        .unwrap();
    let install = runtime.make_closure("install.pause", ()).unwrap();
    runtime
        .globals()
        .raw_set(&mut runtime, "install", install)
        .unwrap();
    runtime.load_main(&compile(b"local co=coroutine.create(function()\nlocal x=1\nreturn x+6\nend) install(co) assert(coroutine.resume(co)) return co").unwrap()).unwrap();
    finish(&mut runtime, false, &HostRegistry::new());
    let thread = runtime.result_values().unwrap().remove(0);
    runtime
        .set_hook(Some(&thread), "reload", HookMask::LINE, 0)
        .unwrap();
    assert_eq!(
        runtime
            .resume_thread(thread.id().unwrap(), 1000, &mut Journal::new())
            .unwrap(),
        StepOutcome::Completed
    );
    assert!(*observed.borrow());
}
