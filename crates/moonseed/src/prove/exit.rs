//! os.exit's runtime mechanism, exposed only as a test global.
use super::*;
use crate::heap::ExitPhase;
use crate::{ApiError, CallOutcome, ExitStatus, Libraries};

fn registry() -> HostRegistry {
    let mut registry = HostRegistry::proof();
    registry.register_builtin("test.exit", crate::host::Builtin::Exit);
    registry
}

fn boot(source: &str) -> Runtime {
    let mut runtime = Runtime::builder()
        .registry(registry())
        .libraries(Libraries::ALL)
        .build()
        .unwrap();
    runtime.set_global_native("_exit", "test.exit").unwrap();
    runtime.set_global_native("park", "park").unwrap();
    runtime
        .load_main(&crate::compile(source.as_bytes()).unwrap())
        .unwrap();
    runtime
}

fn restore(runtime: &Runtime) -> Runtime {
    let bytes = runtime.snapshot().unwrap();
    let restored = Runtime::from_snapshot(&bytes, &registry(), runtime.effect_domain()).unwrap();
    assert_eq!(bytes, restored.snapshot().unwrap());
    restored
}

fn expected(status: ExitStatus, close: bool) -> StepOutcome {
    StepOutcome::ExitRequested { status, close }
}

const STATUS_CASES: &[(&str, ExitStatus)] = &[
    ("", ExitStatus::Success),
    ("nil", ExitStatus::Success),
    ("true", ExitStatus::Success),
    ("false", ExitStatus::Failure),
    ("0", ExitStatus::Code(0)),
    ("7", ExitStatus::Code(7)),
    ("-7", ExitStatus::Code(-7)),
    ("7.0", ExitStatus::Code(7)),
    ("' 7.0 '", ExitStatus::Code(7)),
    ("'0x10'", ExitStatus::Code(16)),
    ("4294967297", ExitStatus::Code(1)),
    ("math.maxinteger", ExitStatus::Code(-1)),
    ("math.mininteger", ExitStatus::Code(0)),
];

#[test]
fn statuses_and_terminal_public_api() {
    for &(argument, status) in STATUS_CASES {
        for close in [false, true] {
            let args = if argument.is_empty() { "nil" } else { argument };
            let mut runtime = boot(&format!("_exit({args}, {close}); error('continued')"));
            let mut journal = Journal::new();
            assert_eq!(
                runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
                expected(status, close)
            );
            assert!(matches!(
                runtime.run(1, &mut journal),
                Err(VmError::Api(ApiError::InvalidCallState))
            ));
            let mut restored = restore(&runtime);
            assert!(matches!(
                restored.run(1, &mut journal),
                Err(VmError::Api(ApiError::InvalidCallState))
            ));
            assert!(
                restored
                    .load_main(&crate::compile(b"return 1").unwrap())
                    .is_err()
            );
        }
    }
    let mut runtime = boot("function quit() _exit(false) end");
    let mut journal = Journal::new();
    assert_eq!(
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
        StepOutcome::Completed
    );
    let function = runtime
        .globals()
        .raw_get::<_, crate::Function>(&mut runtime, "quit")
        .unwrap();
    assert!(matches!(
        runtime
            .call::<()>(&function, (), &mut journal, u64::MAX)
            .unwrap(),
        CallOutcome::ExitRequested {
            status: ExitStatus::Failure,
            close: false
        }
    ));
    assert!(runtime.start_call(&function, ()).is_err());
    assert!(runtime.finish_call::<()>().is_err());
}

const CLOSE_SETUP: &str = r#"
collectgarbage('stop')
local function object(name)
  return setmetatable({}, {
    __close = function(_, err) print('close', name, err) end,
    __gc = function() print('gc', name) end
  })
end
local main <close> = object('main')
local co = coroutine.create(function()
  local child <close> = object('child')
  coroutine.yield()
end)
assert(coroutine.resume(co))
local function nested()
  local inner <close> = object('inner')
  _exit(7, CLOSE)
  print('continued')
end
xpcall(function() pcall(nested); print('caught') end, function() print('handler') end)
print('after')
"#;

#[test]
fn main_closes_then_finalizers_and_false_closes_nothing() {
    for close in [false, true] {
        let mut runtime = boot(&CLOSE_SETUP.replace("CLOSE", &close.to_string()));
        let out = super::base::capture(&mut runtime);
        assert_eq!(
            runtime
                .run_until_terminal(u64::MAX, &mut Journal::new())
                .unwrap(),
            expected(ExitStatus::Code(7), close)
        );
        assert_eq!(
            super::base::text(&out),
            if close {
                "close\tinner\tnil\nclose\tmain\tnil\ngc\tinner\ngc\tchild\ngc\tmain\n"
            } else {
                ""
            }
        );
    }
}

#[test]
fn exit_crosses_coroutines_and_metamethods() {
    let bodies = [
        "pcall(function() pcall(function() _exit() end); print('caught') end)",
        "xpcall(function() _exit() end, function() print('handler') end)",
        "coroutine.resume(coroutine.create(function() _exit() end))",
        "coroutine.wrap(function() coroutine.wrap(function() _exit() end)() end)()",
        "local x = setmetatable({}, {__index = function() _exit() end}); return x.missing",
        "local x = setmetatable({}, {__add = function() _exit() end}); return x + x",
    ];
    for body in bodies {
        for close in [false, true] {
            let source = format!("local m <close> = setmetatable({{}}, {{__close = function() print('main') end}}); {body}")
                .replace("_exit()", &format!("_exit(true, {close})"));
            let mut runtime = boot(&source);
            let out = super::base::capture(&mut runtime);
            assert_eq!(
                runtime
                    .run_until_terminal(u64::MAX, &mut Journal::new())
                    .unwrap(),
                expected(ExitStatus::Success, close),
                "{source}"
            );
            assert_eq!(super::base::text(&out), if close { "main\n" } else { "" });
        }
    }
}

const CLOSE_ERRORS: &str = r#"
collectgarbage('stop')
local outer <close> = setmetatable({}, {__close=function(_, e) print('outer', e) end})
local inner <close> = setmetatable({}, {__close=function() error('close failed', 0) end})
local g = setmetatable({}, {__gc=function() error('gc failed', 0) end})
xpcall(function() _exit(nil, true) end, function() print('handler') end)
"#;

#[test]
fn close_errors_propagate_to_remaining_closes_and_gc_warns() {
    let source = CLOSE_ERRORS;
    let mut runtime = boot(source);
    let out = super::base::capture(&mut runtime);
    let warnings = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = warnings.clone();
    runtime.set_warnings(Box::new(move |bytes, continued| {
        sink.borrow_mut().extend_from_slice(bytes);
        if !continued {
            sink.borrow_mut().push(b'\n');
        }
    }));
    assert_eq!(
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        expected(ExitStatus::Success, true)
    );
    assert_eq!(super::base::text(&out), "outer\tclose failed\n");
    assert_eq!(&*warnings.borrow(), b"error in __gc (gc failed)\n");
}

#[test]
fn checkpoints_during_close_and_finalizer_wait_finish_once() {
    let source = r#"
collectgarbage('stop')
local main <close> = setmetatable({}, {__close=function() print('close-before'); park(); print('close-after') end,
  __gc=function() print('gc-before'); park(); print('gc-after') end})
coroutine.wrap(function() local child <close> = setmetatable({}, {__close=function() error('child closed') end}); _exit(9, true) end)()
"#;
    let mut runtime = boot(source);
    let out = super::base::capture(&mut runtime);
    let mut journal = Journal::new();
    let mut waits = 0;
    let mut saw_scopes = false;
    let mut saw_finalizers = false;
    loop {
        runtime = restore(&runtime);
        let sink = out.clone();
        runtime.set_output(Box::new(move |bytes| {
            sink.borrow_mut().extend_from_slice(bytes)
        }));
        if let Some(exit) = runtime.heap().finalizers.exit {
            saw_scopes |= exit.phase == ExitPhase::Scopes;
            saw_finalizers |= exit.phase == ExitPhase::Finalizers;
        }
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Waiting(key) => {
                runtime = restore(&runtime);
                let sink = out.clone();
                runtime.set_output(Box::new(move |bytes| {
                    sink.borrow_mut().extend_from_slice(bytes)
                }));
                runtime
                    .complete_legacy(key, crate::LegacyCompletion::Return(vec![]))
                    .unwrap();
                assert!(
                    runtime
                        .complete_legacy(key, crate::LegacyCompletion::Return(vec![]))
                        .is_err()
                );
                waits += 1;
            }
            outcome => {
                assert_eq!(outcome, expected(ExitStatus::Code(9), true));
                break;
            }
        }
    }
    assert_eq!(waits, 2);
    assert!(saw_scopes && saw_finalizers);
    assert_eq!(
        super::base::text(&out),
        "close-before\nclose-after\ngc-before\ngc-after\n"
    );
}

#[test]
fn bad_exit_arguments_remain_catchable() {
    let mut runtime = boot(
        r#"
for _, arg in ipairs({1.5, '1.5', 'no', {}, function() end, math.huge}) do
  local ok, message = pcall(_exit, arg)
  assert(not ok and type(message) == 'string')
end
return true
"#,
    );
    assert_eq!(
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Completed
    );
}

#[test]
fn crate_never_calls_process_exit() {
    fn check(path: &std::path::Path) {
        for entry in crate::hostcaps::native::test_support::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                check(&path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let text = crate::hostcaps::native::test_support::read_to_string(&path).unwrap();
                let forbidden = ["std", "process", "exit"].join("::");
                assert!(!text.contains(&forbidden), "{}", path.display());
            }
        }
    }
    check(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
}

#[test]
#[ignore = "lua54_oracle"]
fn lua54_exit_oracle() {
    let lua = crate::hostcaps::native::test_support::var("MOONSEED_LUA54").expect("MOONSEED_LUA54");
    for &(argument, status) in STATUS_CASES {
        let output = super::lua_command("timeout")
            .arg("60")
            .arg(&lua)
            .arg("-e")
            .arg(format!("os.exit({argument})"))
            .output()
            .unwrap();
        let code = match status {
            ExitStatus::Success => 0,
            ExitStatus::Failure => 1,
            ExitStatus::Code(code) => i32::from(code as u8),
        };
        assert_eq!(output.status.code(), Some(code), "{argument}");
        assert!(output.stdout.is_empty() && output.stderr.is_empty());
    }
    let output = super::lua_command("timeout")
        .arg("60")
        .arg(&lua)
        .arg("-e")
        .arg(format!(
            "warn('@on'); {}",
            CLOSE_ERRORS.replace("_exit", "os.exit")
        ))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"outer\tclose failed\n");
    assert_eq!(output.stderr, b"Lua warning: error in __gc (gc failed)\n");
    for close in [false, true] {
        let source = CLOSE_SETUP.replace("CLOSE", &close.to_string());
        let output = super::lua_command("timeout")
            .arg("60")
            .arg(&lua)
            .arg("-e")
            .arg(source.replace("_exit", "os.exit"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(7));
        let mut runtime = boot(&source);
        let out = super::base::capture(&mut runtime);
        assert_eq!(
            runtime
                .run_until_terminal(u64::MAX, &mut Journal::new())
                .unwrap(),
            expected(ExitStatus::Code(7), close)
        );
        assert_eq!(*out.borrow(), output.stdout);
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn exit_snapshot_rejects_inconsistent_shutdown_state() {
    let mut runtime =
        boot("local x <close> = setmetatable({}, {__close=function() park() end}); _exit(3, true)");
    assert!(matches!(
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Waiting(_)
    ));
    let image = runtime.to_image().unwrap();
    for mutation in 0..4 {
        let mut bad = image.clone();
        match mutation {
            0 => bad.finalizers.exit = None,
            1 => bad.finalizers.exit.as_mut().unwrap().close = false,
            2 => bad.finalizers.exit.as_mut().unwrap().phase = ExitPhase::Terminal,
            3 => bad.finalizers.exit.as_mut().unwrap().phase = ExitPhase::Finalizers,
            _ => unreachable!(),
        }
        let bytes = snapshot::encode(&bad).unwrap();
        assert!(matches!(
            Runtime::from_snapshot(&bytes, &registry(), runtime.effect_domain()),
            Err(SnapshotError::InvalidStructure)
        ));
    }
}

#[test]
fn close_truthiness_and_calls_with_exit_outcomes() {
    for (arg, closes) in [
        ("nil", false),
        ("false", false),
        ("0", true),
        ("''", true),
        ("{}", true),
    ] {
        let mut runtime = boot(&format!("function quit() _exit(nil, {arg}) end"));
        let mut journal = Journal::new();
        assert_eq!(
            runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
            StepOutcome::Completed
        );
        let function = runtime
            .globals()
            .raw_get::<_, crate::Function>(&mut runtime, "quit")
            .unwrap();
        assert!(
            matches!(runtime.call::<()>(&function, (), &mut journal, u64::MAX).unwrap(),
            CallOutcome::ExitRequested { status: ExitStatus::Success, close } if close == closes)
        );
    }
}

#[test]
fn coroutine_host_wait_inside_main_close_restores() {
    let mut runtime = boot(
        "local x <close> = setmetatable({}, {__close=function() coroutine.wrap(function() park(); print('closed') end)() end}); _exit(nil, true)",
    );
    let mut journal = Journal::new();
    let StepOutcome::Waiting(key) = runtime.run_until_terminal(u64::MAX, &mut journal).unwrap()
    else {
        panic!("no wait");
    };
    let mut runtime = restore(&runtime);
    let out = super::base::capture(&mut runtime);
    runtime
        .complete_legacy(key, crate::LegacyCompletion::Return(vec![]))
        .unwrap();
    assert_eq!(
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
        expected(ExitStatus::Success, true)
    );
    assert_eq!(super::base::text(&out), "closed\n");
}
