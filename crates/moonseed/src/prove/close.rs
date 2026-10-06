//! To-be-closed variables, scope closing, and thread close (ADR 0026). The
//! source fixtures that match Lua 5.4.9 are in `NATIVE_FIXTURES`.

use super::*;
use crate::program::{CloseCase, close_case_program};

fn compile_error(source: &str) -> crate::CompileError {
    match crate::compile(source.as_bytes()) {
        Err(error) => error,
        Ok(_) => panic!("compiled: {source}"),
    }
}

#[test]
fn close_locals_parse_once_per_list_and_are_read_only() {
    for source in [
        "local x <close> = nil x = 1",
        "local x <close> = nil local f = function() x = 1 end",
        "local x <close> = nil local f = function() return function() x = 1 end end",
        "local y local x <close> = nil y, x = 1, 2",
    ] {
        let error = compile_error(source);
        assert_eq!(error.kind, crate::CompileErrorKind::Syntax, "{source}");
        assert!(error.message.contains("const variable 'x'"), "{source}");
    }
    let error = compile_error("local a <close>, b <close> = nil, nil");
    assert_eq!(error.kind, crate::CompileErrorKind::Syntax);
    assert!(error.message.contains("multiple to-be-closed"));
    assert_eq!(
        compile_error("local a <other> = 1").kind,
        crate::CompileErrorKind::Syntax
    );
    // The value the local refers to may still change, and the name is not
    // visible in its own initializer.
    let chunk = crate::compile(
        b"local x = 5 \
          do local x <close> = setmetatable({ v = x }, { __close = function() end }) x.v = x.v + 1 \
          return x.v end",
    )
    .unwrap();
    let runtime = finish_with(boot_natives, &chunk.proto);
    assert_eq!(print_line(&runtime), "6\n");
}

/// Each case with Lua 5.4.9's answer for the same program written with the
/// coroutine library.
const CASES: [(CloseCase, &str); 9] = [
    (CloseCase::ScopeYield, "a\tdone\ta\tnil\tnil\tnil\n"),
    (CloseCase::ReturnYield, "a\t10\t20\t30\ta\tnil\tnil\tnil\n"),
    (CloseCase::UnwindYield, "a\tfalse\tE\ta\tE\tnil\tnil\n"),
    (
        CloseCase::ErrorThenClose,
        "false\tboom\t0\tfalse\tboom\ttrue\tnil\ta\tboom\tnil\tnil\n",
    ),
    (
        CloseCase::TailError,
        "false\tboom\t0\tfalse\tboom\ttrue\tnil\ta\tboom\tnil\tnil\n",
    ),
    (CloseCase::SuspendedClose, "true\tnil\tb\ta\tnil\tnil\n"),
    (CloseCase::CloseError, "false\teb\tb\ta\teb\tnil\n"),
    (
        CloseCase::CloseYield,
        "false\tattempt to yield across a C-call boundary\tb\ta\tnil\tnil\n",
    ),
    (CloseCase::InnerPcall, "false\tea\ta\tnil\tnil\tnil\n"),
];

#[test]
fn coroutine_closes_match_lua_under_every_schedule() {
    for (case, line) in CASES {
        let spec = close_case_program(case);
        let runtime = finish_with(boot_natives, &spec);
        assert_eq!(print_line(&runtime), line, "{case:?}");
        let expected = pair_results(&runtime);
        quantum_and_checkpoints_with(boot_natives, &spec, pair_results);
        collect_every_safe_point_with(boot_natives, &spec, &expected);
    }
}

#[test]
fn a_failed_coroutine_keeps_its_stack_until_it_is_closed() {
    let spec = close_case_program(CloseCase::ErrorThenClose);
    let mut runtime = boot_natives(&spec);
    let mut journal = Journal::new();
    // Run until the coroutine has failed, before the first `CloseThread`.
    let failed = |runtime: &Runtime| {
        runtime
            .heap()
            .threads
            .iter()
            .any(|(_, _, thread)| thread.status == crate::heap::Status::Failed)
    };
    while !failed(&runtime) {
        runtime.run(1, &mut journal).unwrap();
    }
    let heap = runtime.heap();
    let (_, _, thread) = heap
        .threads
        .iter()
        .find(|(_, _, thread)| thread.status == crate::heap::Status::Failed)
        .unwrap();
    assert!(thread.coroutine);
    assert!(!thread.frames.is_empty(), "the failed stack was unwound");
    assert_eq!(thread.tbc.len(), 1, "the <close> value is still pending");
    // The failed state is snapshot state, and finishes the same restored.
    let mut restored = Runtime::from_snapshot(
        &runtime.snapshot().unwrap(),
        &HostRegistry::proof(),
        runtime.effect_domain(),
    )
    .unwrap();
    assert_eq!(
        restored
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(
        print_line(&restored),
        "false\tboom\t0\tfalse\tboom\ttrue\tnil\ta\tboom\tnil\tnil\n"
    );
}

/// An External native `__close`: commits one journal effect, whatever its
/// arguments.
fn close_effect(call: &mut crate::host::NativeCall<'_>) -> crate::host::NativeOutcome {
    let Some(effect) = call.effect() else {
        return crate::host::NativeOutcome::Fault;
    };
    let Some(journal) = call.journal() else {
        return crate::host::NativeOutcome::Fault;
    };
    journal.commit(effect, 7, || 7).unwrap();
    crate::host::NativeOutcome::Ready
}

#[test]
fn native_closes_run_once_under_checkpoints() {
    let mut registry = HostRegistry::proof();
    registry.register_native(
        "close.effect",
        crate::host::NativePolicy::External,
        close_effect,
    );
    let chunk = crate::compile(
        b"local n = 0 \
          do local a <close> = setmetatable({}, { __close = second }) \
             local b <close> = setmetatable({}, { __close = effect }) \
             local c <close> = setmetatable({}, { __close = function() n = n + 1 end }) end \
          return n",
    )
    .unwrap();
    let boot = || {
        let mut runtime =
            Runtime::boot(Config::default(), registry.clone(), &chunk.proto, false).unwrap();
        runtime.install_base().unwrap();
        runtime.set_global_native("second", "second").unwrap();
        runtime.set_global_native("effect", "close.effect").unwrap();
        runtime
    };
    // Uninterrupted, then restored at every step onto the same journal.
    let mut runtime = boot();
    let mut journal = Journal::new();
    assert_eq!(
        runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(print_line(&runtime), "1\n");
    assert_eq!(journal.entries().len(), 1);
    let mut runtime = boot();
    let mut journal = Journal::new();
    loop {
        let bytes = runtime.snapshot().unwrap();
        runtime = Runtime::from_snapshot(&bytes, &registry, runtime.effect_domain()).unwrap();
        match runtime.run(1, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(print_line(&runtime), "1\n");
    assert_eq!(journal.entries().len(), 1);
}

#[test]
fn closes_run_through_memory_errors_and_stack_overflow() {
    // A memory error with two closes pending: the newer allocates past the
    // quota again, and the older still runs and sees that error. The
    // frames stay alive while they close, so the older one records it in
    // an upvalue, which allocates nothing.
    let config = Config {
        max_logical_heap: 256 * 1024,
        ..Config::default()
    };
    let chunk = crate::compile(
        b"local saw \
          local grow = function() \
            local a <close> = setmetatable({}, { __close = function(_, err) saw = err end }) \
            local b <close> = setmetatable({}, { __close = function() local t = {} for i = 1, 100000 do t[i] = i end end }) \
            local keep = {} for i = 1, 100000 do keep[i] = i end \
          end \
          local ok, err = pcall(grow) \
          return ok, err, saw",
    )
    .unwrap();
    let mut runtime = Runtime::boot(config, HostRegistry::proof(), &chunk.proto, false).unwrap();
    runtime.install_base().unwrap();
    assert_eq!(
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(
        print_line(&runtime),
        "false\tnot enough memory\tnot enough memory\n"
    );
    assert!(runtime.memory().logical_bytes <= 256 * 1024);

    // Every frame of an overflowing recursion holds a `<close>` value; each
    // closes while the unwind passes, its call in the frames kept in
    // reserve past the depth bound.
    let chunk = crate::compile(
        b"local closed = 0 local f \
          f = function(n) \
            local c <close> = setmetatable({}, { __close = function() closed = closed + 1 end }) \
            return (f(n + 1)) \
          end \
          local ok, e = pcall(f, 1) \
          return ok, e, closed",
    )
    .unwrap();
    let runtime = finish_with(boot_natives, &chunk.proto);
    assert_eq!(print_line(&runtime), "false\t?:1: stack overflow\t998\n");
}

#[test]
fn a_closed_value_is_collectable_once_its_scope_is_left() {
    let with_metatable = |runtime: &Runtime| {
        runtime
            .heap()
            .tables
            .iter()
            .filter(|(_, _, table)| table.metatable.is_some())
            .count()
    };
    for (source, live) in [
        (
            &b"do local x <close> = setmetatable({}, { __close = function() end }) end park()"[..],
            0,
        ),
        (
            b"local x <close> = setmetatable({}, { __close = function() end }) park()",
            1,
        ),
    ] {
        let mut runtime = run_to_wait(source);
        runtime.collect();
        assert_eq!(
            with_metatable(&runtime),
            live,
            "{}",
            String::from_utf8_lossy(source)
        );
    }
}

#[test]
fn restore_refuses_impossible_close_states() {
    use crate::snapshot::{EventImage, Image, MetaImage, NextImage};
    let check = |runtime: &Runtime, change: &dyn Fn(&mut Image)| {
        let mut image = runtime.to_image().unwrap();
        change(&mut image);
        let bytes = snapshot::encode(&image).unwrap();
        expect_snapshot(
            Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()),
            SnapshotError::InvalidStructure,
        );
    };
    // Edit the event of the close in progress.
    fn edit_close(image: &mut Image, edit: &dyn Fn(&mut EventImage)) {
        let meta: &mut MetaImage = image
            .threads
            .iter_mut()
            .flat_map(|thread| thread.frames.iter_mut())
            .find_map(|frame| {
                frame
                    .meta
                    .as_mut()
                    .filter(|meta| matches!(meta.event, EventImage::Close { .. }))
            })
            .expect("a close in progress");
        edit(&mut meta.event);
    }
    let entry = |image: &mut Image| -> usize {
        image
            .threads
            .iter()
            .position(|thread| !thread.tbc.is_empty())
            .expect("a to-be-closed value")
    };

    // Waiting in a scope's close, with an older value still listed.
    let scope = run_to_wait(
        b"do local a <close> = setmetatable({}, { __close = function() end }) \
          local b <close> = setmetatable({}, { __close = park }) end",
    );
    // Untampered, it restores.
    Runtime::from_snapshot(
        &scope.snapshot().unwrap(),
        &HostRegistry::proof(),
        scope.effect_domain(),
    )
    .unwrap();
    // A listed slot outside every frame, listed twice, or out of order.
    check(&scope, &|image| {
        let t = entry(image);
        image.threads[t].tbc[0] = 9_000;
    });
    check(&scope, &|image| {
        let t = entry(image);
        let slot = image.threads[t].tbc[0];
        image.threads[t].tbc.push(slot);
    });
    check(&scope, &|image| {
        let t = entry(image);
        let slot = image.threads[t].tbc[0];
        image.threads[t].tbc.insert(0, slot + 1);
    });
    // A scope close that does not match its `CloseScope`, or that claims to
    // be a return.
    check(&scope, &|image| {
        edit_close(image, &|event| {
            if let EventImage::Close { from, .. } = event {
                *from += 1;
            }
        })
    });
    check(&scope, &|image| {
        edit_close(image, &|event| {
            if let EventImage::Close { next, .. } = event {
                *next = NextImage::Return {
                    src: 0,
                    produced: 0,
                };
            }
        })
    });

    // Waiting in a return's close: the result window must be the `Return`'s.
    let ret = run_to_wait(
        b"local f = function() local b <close> = setmetatable({}, { __close = park }) return 1, 2 end \
          return f()",
    );
    check(&ret, &|image| {
        edit_close(image, &|event| {
            if let EventImage::Close {
                next: NextImage::Return { produced, .. },
                ..
            } = event
            {
                *produced += 1;
            }
        })
    });

    // Waiting in an unwind's close: the unwind must carry an error, and aim
    // at the nearest protected call.
    let unwind = run_to_wait(
        b"local ok = pcall(function() local b <close> = setmetatable({}, { __close = park }) error('x', 0) end)",
    );
    check(&unwind, &|image| {
        edit_close(image, &|event| {
            if let EventImage::Close {
                next: NextImage::Unwind(unwind),
                ..
            } = event
            {
                unwind.error = None;
            }
        })
    });
    check(&unwind, &|image| {
        edit_close(image, &|event| {
            if let EventImage::Close {
                next: NextImage::Unwind(unwind),
                ..
            } = event
            {
                unwind.phase = crate::heap::UnwindPhase::Popping { target: None };
            }
        })
    });

    // A thread marked as being closed that is not a coroutine with a closer
    // waiting for it, and a failed entry thread that kept its frames.
    check(&scope, &|image| {
        let t = entry(image);
        image.threads[t].closing = true;
    });
    let failed = {
        let spec = close_case_program(CloseCase::ErrorThenClose);
        let mut runtime = boot_natives(&spec);
        let mut journal = Journal::new();
        while !runtime
            .heap()
            .threads
            .iter()
            .any(|(_, _, thread)| thread.status == crate::heap::Status::Failed)
        {
            runtime.run(1, &mut journal).unwrap();
        }
        runtime
    };
    check(&failed, &|image| {
        for thread in &mut image.threads {
            if thread.status == crate::heap::Status::Failed.tag() {
                thread.coroutine = false;
            }
        }
    });
}

#[test]
fn a_thread_close_can_wait_on_the_host_and_restore() {
    let spec = crate::program::thread_close_wait_program();
    for (completion, line) in [
        (
            crate::host::LegacyCompletion::Return(vec![crate::host::HostValue::Integer(7)]),
            "true\tnil\n",
        ),
        // The coroutine's own `pcall` must not catch the close's error.
        (
            crate::host::LegacyCompletion::Error(crate::host::HostValue::String(b"E".to_vec())),
            "false\tE\n",
        ),
    ] {
        let mut runtime = boot_natives(&spec);
        let mut journal = Journal::new();
        assert_eq!(
            runtime.run_until_terminal(u64::MAX, &mut journal).unwrap(),
            StepOutcome::Waiting(WaitKey(1))
        );
        // The waiting close is snapshot state.
        let mut restored = Runtime::from_snapshot(
            &runtime.snapshot().unwrap(),
            &HostRegistry::proof(),
            runtime.effect_domain(),
        )
        .unwrap();
        restored.complete_legacy(WaitKey(1), completion).unwrap();
        // Every later state restores too, and the result is the same.
        loop {
            restored = Runtime::from_snapshot(
                &restored.snapshot().unwrap(),
                &HostRegistry::proof(),
                restored.effect_domain(),
            )
            .unwrap();
            match restored.run(1, &mut journal).unwrap() {
                StepOutcome::Paused(_) => {}
                StepOutcome::Completed => break,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(print_line(&restored), line);
    }
}

#[test]
fn nested_coroutines_fail_and_close_as_in_lua() {
    // A resumes B; B fails holding a value; A fails with B's error, which
    // leaves A's `Resume` over; B is closed later. Lua 5.4.9 gives the same
    // for the program written with the coroutine library.
    let spec = crate::program::nested_coroutine_program(false);
    let runtime = finish_with(boot_natives, &spec);
    assert_eq!(print_line(&runtime), "false\tboom\tfalse\tboom\tb\tboom\n");
    let expected = pair_results(&runtime);
    quantum_and_checkpoints_with(boot_natives, &spec, pair_results);
    collect_every_safe_point_with(boot_natives, &spec, &expected);
    // B closing A, which resumed it, is Lua's "normal" coroutine error.
    let spec = crate::program::nested_coroutine_program(true);
    let runtime = finish_with(boot_natives, &spec);
    assert_eq!(
        print_line(&runtime),
        "false\tcannot close a normal coroutine\n"
    );
}

#[test]
fn closes_past_the_depth_bound_fail_as_error_handling() {
    // Each frame's close recurses without end while a stack overflow is
    // being unwound: it runs out of the reserve, which Lua 5.4.9 reports as
    // "error in error handling".
    let chunk = crate::compile(
        b"local deep deep = function(n) if n == 0 then return 0 end return (deep(n - 1)) end \
          local f f = function(n) \
            local c <close> = setmetatable({}, { __close = function() deep(100000) end }) \
            return (f(n + 1)) \
          end \
          return pcall(f, 1)",
    )
    .unwrap();
    let runtime = finish_with(boot_natives, &chunk.proto);
    assert_eq!(print_line(&runtime), "false\terror in error handling\n");
    // A `pcall` inside such a close works in the reserve: no close aborts.
    let chunk = crate::compile(
        b"local deep deep = function(n) if n == 0 then return 0 end return (deep(n - 1)) end \
          local done = 0 local f \
          f = function(n) \
            local c <close> = setmetatable({}, { __close = function() pcall(deep, 3) done = done + 1 end }) \
            return (f(n + 1)) \
          end \
          local ok, e = pcall(f, 1) \
          return ok, e, done",
    )
    .unwrap();
    let runtime = finish_with(boot_natives, &chunk.proto);
    assert_eq!(print_line(&runtime), "false\t?:1: stack overflow\t998\n");
}

#[test]
fn restore_refuses_close_states_no_run_reaches() {
    use crate::snapshot::{EventImage, Image, UnwindImage};
    let check = |runtime: &Runtime, change: &dyn Fn(&mut Image)| {
        let mut image = runtime.to_image().unwrap();
        change(&mut image);
        let bytes = snapshot::encode(&image).unwrap();
        expect_snapshot(
            Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()),
            SnapshotError::InvalidStructure,
        );
    };
    // A frame idle between closes with a call running above it, and no
    // error that would explain it.
    let running = run_to_wait(
        b"local f = function() local b <close> = setmetatable({}, { __close = function() park() end }) return 1 end \
          return f()",
    );
    check(&running, &|image| {
        for thread in &mut image.threads {
            for frame in &mut thread.frames {
                if let Some(meta) = &mut frame.meta
                    && matches!(meta.event, EventImage::Close { .. })
                {
                    meta.phase = crate::heap::MetaPhase::Idle;
                    meta.slot = 0;
                    meta.nargs = 0;
                }
            }
        }
    });
    // The entry thread marked as a coroutine.
    check(&running, &|image| {
        let entry = image.entry;
        for thread in &mut image.threads {
            if thread.id == entry {
                thread.coroutine = true;
            }
        }
    });
    // A thread close whose new error aims at a `pcall` below the frame
    // being closed, which would catch it.
    let spec = crate::program::thread_close_wait_program();
    let mut closing = boot_natives(&spec);
    closing
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    closing
        .complete_legacy(
            WaitKey(1),
            crate::host::LegacyCompletion::Error(crate::host::HostValue::String(b"E".to_vec())),
        )
        .unwrap();
    closing.unwind_one_step().unwrap();
    Runtime::from_snapshot(
        &closing.snapshot().unwrap(),
        &HostRegistry::proof(),
        closing.effect_domain(),
    )
    .unwrap();
    check(&closing, &|image| {
        for thread in &mut image.threads {
            let protect = thread.frames.iter().position(|frame| {
                matches!(
                    frame.boundary,
                    Some(crate::snapshot::BoundaryImage::Protect { .. })
                )
            });
            if let (Some(UnwindImage { phase, .. }), Some(protect)) =
                (thread.unwind.as_mut(), protect)
                && thread.closing
            {
                *phase = crate::heap::UnwindPhase::Popping {
                    target: Some(protect as u32),
                };
            }
        }
    });
}
