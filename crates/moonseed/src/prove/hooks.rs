//! Lua hook schedules, fuel, GC rooting and negative recovery states (H1).
use super::*;
use crate::host::HostValue;
use crate::runtime::{HotCoreMode, hooks::Event};

fn boot(source: &str, gc_mode: crate::GcMode) -> Runtime {
    let chunk = crate::compile(source.as_bytes()).unwrap();
    let mut runtime = Runtime::boot(
        Config {
            gc_mode,
            fuel_limit: None,
            ..Config::default()
        },
        HostRegistry::proof(),
        &chunk.proto,
        false,
    )
    .unwrap();
    runtime.install_standard().unwrap();
    runtime.install_debug().unwrap();
    runtime
}
fn run(runtime: &mut Runtime, quantum: u64) {
    let mut journal = Journal::new();
    for _ in 0..100_000 {
        match runtime.run(quantum, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => return,
            other => panic!("{other:?}: {:?}", runtime.lua_error()),
        }
    }
    panic!("hook schedule failed to finish");
}
fn run_checkpoint(runtime: &mut Runtime, quantum: u64) {
    let mut journal = Journal::new();
    for step in 0..100_000 {
        let bytes = runtime.snapshot().unwrap();
        let mode = runtime.hot_core;
        let registry = runtime.host_registry().clone();
        *runtime = Runtime::from_snapshot(&bytes, &registry, 1).unwrap_or_else(|e| {
            panic!(
                "restore step {step}: {e:?}: {:#?}",
                runtime.to_image().unwrap().threads
            )
        });
        runtime.hot_core = mode;
        crate::gc::check_usage(runtime.heap()).unwrap();
        match runtime.run(quantum, &mut journal).unwrap() {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => return,
            other => panic!("{other:?}: {:?}", runtime.lua_error()),
        }
    }
    panic!("checkpoint schedule failed to finish");
}
fn state(runtime: &Runtime) -> Vec<(u64, u8, i32, i32, bool)> {
    let mut result: Vec<_> = runtime
        .heap()
        .threads
        .iter()
        .filter_map(|(_, _, t)| {
            runtime.heap().hooks.get(t.id).map(|h| {
                (
                    t.id.raw(),
                    h.mask,
                    h.base_count,
                    h.remaining_count,
                    h.allow_hook,
                )
            })
        })
        .collect();
    result.sort_unstable();
    result
}

#[test]
fn hook_determinism_matrix() {
    let bodies = [
        "local x=0 for i=1,4 do x=x+i end",
        "local x=0 while x<4 do x=x+1 end",
        "local x=0 repeat x=x+1 until x==4",
        "local x=0 for _,v in ipairs({1,2,3}) do x=x+v end",
        "local x=0 ::again:: x=x+1 if x<3 then goto again end",
        "local f=function(a,b) return a,nil,b end local x={f(1,2)}",
        "local f; f=function(n) if n==0 then return 3 end return f(n-1) end local x=f(4)",
        "local f=function(...) return select('#',...),... end local x={f(1,nil,3)}",
        "local t=setmetatable({}, {__index=function(_,k) return k end}) local x=t.foo",
        "local t=setmetatable({}, {__add=function() return 3 end}) local x=t+1",
        "local t=setmetatable({}, {__call=function() return 3 end}) local x=t()",
        "local x=pcall(function() return 3 end)",
        "local x=pcall(function() error('body',0) end)",
        "local x=xpcall(function() error('body',0) end,function(e) return e end)",
        "local co=coroutine.create(function() coroutine.yield(1) return 2 end) coroutine.resume(co) coroutine.resume(co)",
        "local x=math.sin(1) local y=select(2,1,nil,3)",
        "local t={3,1,2} table.sort(t,function(a,b) return a<b end)",
        "local x=string.gsub('abc','.',function(c) return c end)",
        "do local a <close> = setmetatable({}, {__close=function() end}) end",
        "local f=function() local a <close> = setmetatable({}, {__close=function() end}) return 1,nil,3 end local x={f()}",
        "local x=false and 3 or 4",
        "local x=0 for i=1,4 do if i==3 then break end x=x+i end",
        "local x=load('return 3')()",
        "local x=debug.gethook()",
        "local co=coroutine.create(function() return 3 end) debug.sethook(co,debug.gethook(),'crl',4) coroutine.resume(co)",
        "collectgarbage('collect')",
        "local x=pcall(function() local a <close> = setmetatable({}, {__close=function() end}) error('close',0) end)",
        "local t=setmetatable({}, {__tostring=function() return 't' end}) local x=tostring(t)",
        "local co=coroutine.create(function() coroutine.yield(1) end) coroutine.resume(co) coroutine.close(co)",
        "local x=table.unpack({1,2,3})",
    ];
    assert_eq!(bodies.len(), 30);
    for (id, body) in bodies.iter().enumerate() {
        let source = format!(
            "local trace={{}} local function h(e,l) trace[#trace+1]=e..':'..tostring(l) end debug.sethook(h,'crl',4); {body}; return table.concat(trace,'|')"
        );
        let mut baseline = boot(&source, crate::GcMode::Incremental);
        run(&mut baseline, u64::MAX);
        let expected = baseline.results().unwrap();
        let expected_state = state(&baseline);

        for gc_mode in [crate::GcMode::Incremental, crate::GcMode::Generational] {
            let mut reference = boot(&source, gc_mode);
            run(&mut reference, u64::MAX);
            let fuel = reference.fuel_consumed();
            for mode in [
                HotCoreMode::Full,
                HotCoreMode::NoFastCalls,
                HotCoreMode::Off,
            ] {
                for quantum in [1, 2, 3, 7] {
                    let mut runtime = boot(&source, gc_mode);
                    runtime.hot_core = mode;
                    run_checkpoint(&mut runtime, quantum);
                    assert_eq!(
                        runtime.results().unwrap(),
                        expected,
                        "program {id}, {mode:?}, {gc_mode:?}, q={quantum}"
                    );
                    assert_eq!(state(&runtime), expected_state, "program {id}, q={quantum}");
                    assert_eq!(
                        runtime.fuel_consumed(),
                        fuel,
                        "program {id}, {mode:?}, {gc_mode:?}, q={quantum}"
                    );
                }
            }
        }
    }
}

#[test]
fn hook_error_restores_suppression_and_keeps_installation() {
    for event in ["call", "return", "line", "count", "tail call"] {
        let source = format!(
            "local once=true local fired=0 local function h(e) if once and e=='{event}' then once=false error('hook',0) end fired=fired+1 end local ok=pcall(function() debug.sethook(h,'crl',1) local f=function() return 3 end f() return f() end) local function f() return 4 end f() return ok, fired>0,debug.gethook()==h"
        );
        for quantum in [1, 2, 3, 7] {
            let mut runtime = boot(&source, crate::GcMode::Incremental);
            run_checkpoint(&mut runtime, quantum);
            assert_eq!(
                runtime.results().unwrap(),
                vec![
                    HostValue::Boolean(false),
                    HostValue::Boolean(true),
                    HostValue::Boolean(true)
                ],
                "{event}"
            );
            for (_, _, thread) in runtime.heap().threads.iter() {
                if let Some(hook) = runtime.heap().hooks.get(thread.id) {
                    assert!(hook.allow_hook);
                    assert!(hook.pending.is_none());
                }
            }
        }
    }
}

#[test]
fn pending_hook_survives_quantum_pause_and_snapshot_restore() {
    for quantum in [1, 2, 3, 7] {
        let mut runtime = boot(
            "local n=0 debug.sethook(function() n=n+1 end,'crl',1) local f=function() return 3 end f() return n",
            crate::GcMode::Incremental,
        );
        assert!(runtime.snapshot().is_ok());
        let mut journal = Journal::new();
        let mut found = false;
        for _ in 0..10_000 {
            assert!(matches!(
                runtime.run(1, &mut journal).unwrap(),
                StepOutcome::Paused(_)
            ));
            found = runtime.heap().threads.iter().any(|(_, _, t)| {
                runtime
                    .heap()
                    .hooks
                    .get(t.id)
                    .is_some_and(|h| h.pending.is_some())
            });
            if found {
                break;
            }
        }
        assert!(found);
        let fuel = runtime.fuel_consumed();
        assert!(matches!(
            runtime.run(0, &mut journal).unwrap(),
            StepOutcome::Paused(_)
        ));
        assert_eq!(runtime.fuel_consumed(), fuel);
        runtime = Runtime::from_snapshot(&runtime.snapshot().unwrap(), &HostRegistry::proof(), 1)
            .unwrap();
        run(&mut runtime, quantum);
        for (_, _, thread) in runtime.heap().threads.iter() {
            let hook = runtime.heap().hooks.get(thread.id).unwrap();
            assert!(hook.allow_hook);
            assert!(hook.pending.is_none());
        }
    }
}

#[test]
fn delivery_costs_exactly_one_fuel_unit() {
    let mut runtime = boot(
        "debug.sethook(function() end,'c') local function f() return 3 end f() return 1",
        crate::GcMode::Incremental,
    );
    let mut journal = Journal::new();
    for _ in 0..1000 {
        runtime.run(1, &mut journal).unwrap();
        let pending = runtime
            .heap()
            .active
            .and_then(|t| runtime.heap().threads.get(t))
            .and_then(|t| runtime.heap().hooks.get(t.id))
            .and_then(|h| h.pending);
        if pending.is_some_and(|e| e.event == Event::Call) {
            let before = runtime.fuel_consumed();
            assert!(matches!(
                runtime.run(1, &mut journal).unwrap(),
                StepOutcome::Paused(_)
            ));
            assert_eq!(runtime.fuel_consumed(), before + 1);
            let hook = runtime
                .heap()
                .active
                .and_then(|t| runtime.heap().threads.get(t))
                .and_then(|t| runtime.heap().hooks.get(t.id))
                .unwrap();
            assert!(!hook.allow_hook);
            assert!(hook.pending.is_none());
            return;
        }
    }
    panic!("no pending call event");
}

#[test]
fn hook_target_is_a_thread_root_and_children_inherit_only_the_wrapper() {
    let mut runtime = boot(
        "local n,child=0,0 local co debug.sethook(function() n=n+1 if coroutine.running()==co then child=child+1 end end,'crl',1) collectgarbage('collect') co=coroutine.create(function() local h,m,c=debug.gethook() assert(h==nil and m=='crl' and c==1) local grand=coroutine.create(function() return 4 end) local gh,gm,gc=debug.gethook(grand) assert(gh==nil and gm=='crl' and gc==1) assert(coroutine.resume(grand)) return 3 end) local h,m,c=debug.gethook(co) assert(h==nil and m=='crl' and c==1) assert(coroutine.resume(co)) return n>0 and child==0",
        crate::GcMode::Generational,
    );
    run(&mut runtime, 1);
    assert_eq!(runtime.results().unwrap(), vec![HostValue::Boolean(true)]);
}

#[test]
fn newly_installed_hooks_observe_already_entered_native_returns() {
    let source = "local p,t=0,0 local function h(e) if e=='return' then local n=debug.getinfo(2,'n').name if n=='pcall' then p=p+1 elseif n=='tostring' then t=t+1 end end end pcall(function() debug.sethook(h,'r') return 3 end) debug.sethook() tostring(setmetatable({}, {__tostring=function() debug.sethook(h,'r') return 't' end})) debug.sethook() return p,t";
    for quantum in [1, 2, 3, 7] {
        let mut runtime = boot(source, crate::GcMode::Incremental);
        run(&mut runtime, quantum);
        assert_eq!(
            runtime.results().unwrap(),
            vec![HostValue::Integer(1), HostValue::Integer(1)]
        );
    }
}

#[test]
fn count_hooks_cannot_yield_and_failed_coroutines_restore_suppression() {
    let source = "local co=coroutine.create(function() local n=0 for i=1,3 do n=n+i end return n end) debug.sethook(co,function() coroutine.yield() end,'',1) local ok,e=coroutine.resume(co) return ok,e=='attempt to yield across a C-call boundary',type(debug.gethook(co))";
    for quantum in [1, 2, 3, 7] {
        let mut runtime = boot(source, crate::GcMode::Incremental);
        run_checkpoint(&mut runtime, quantum);
        assert_eq!(
            runtime.results().unwrap(),
            vec![
                HostValue::Boolean(false),
                HostValue::Boolean(true),
                HostValue::String(b"function".to_vec())
            ]
        );
        for (_, _, thread) in runtime.heap().threads.iter() {
            if let Some(hook) = runtime.heap().hooks.get(thread.id) {
                assert!(hook.allow_hook);
                assert!(hook.pending.is_none());
            }
            assert!(
                runtime
                    .heap()
                    .hooks
                    .get(thread.id)
                    .is_none_or(|hook| hook.transfer.is_none())
            );
        }
    }
}

#[test]
fn hook_side_table_does_not_root_dead_threads() {
    for mode in [crate::GcMode::Incremental, crate::GcMode::Generational] {
        let mut runtime = boot(
            "local weak=setmetatable({}, {__mode='v'}) do local co=coroutine.create(function() end) weak[1]=co debug.sethook(co,function() end,'crl',1) end collectgarbage('collect') collectgarbage('collect') return weak[1]==nil",
            mode,
        );
        run(&mut runtime, 1);
        assert_eq!(runtime.results().unwrap(), vec![HostValue::Boolean(true)]);
        assert!(runtime.heap().hooks.is_empty());
        crate::gc::check_usage(runtime.heap()).unwrap();
        assert!(runtime.snapshot().is_ok());
    }
}

#[test]
fn host_preemption_checkpoint_transitions() {
    use crate::{HookAction, HookEvent, HookMask};
    let chunk = crate::compile(b"local co=coroutine.create(function() install() local a=0 for i=1,4 do a=a+i end local t={a,nil,7} return table.unpack(t,1,3) end) local r repeat r=table.pack(coroutine.resume(co,99)) until coroutine.status(co)=='dead' assert(r[1]) return table.unpack(r,2,r.n)").unwrap();
    for (mask, count) in [
        (HookMask::NONE, 1),
        (HookMask::LINE, 0),
        (HookMask::LINE, 1),
        (HookMask::CALL | HookMask::RETURN, 2),
    ] {
        for gc_mode in [crate::GcMode::Incremental, crate::GcMode::Generational] {
            let mut expected = None;
            for checkpoint in [false, true] {
                for quantum in [1, 2, 3, 7] {
                    let trace = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
                    let observed = trace.clone();
                    let mut registry = HostRegistry::proof();
                    registry.register_hook("preempt", move |cx| {
                        observed.borrow_mut().push((cx.event(), cx.line()));
                        Ok(
                            if matches!(cx.event(), HookEvent::Line | HookEvent::Count) {
                                HookAction::Yield
                            } else {
                                HookAction::Continue
                            },
                        )
                    });
                    registry.typed("install", crate::NativePolicy::VmLocal, move |cx, ()| {
                        cx.set_hook(None, "preempt", mask, count)
                    });
                    let mut runtime = Runtime::boot(
                        Config {
                            gc_mode,
                            ..Config::default()
                        },
                        registry,
                        &chunk.proto,
                        false,
                    )
                    .unwrap();
                    runtime.install_standard().unwrap();
                    runtime.install_debug().unwrap();
                    runtime.set_global_native("install", "install").unwrap();
                    if checkpoint {
                        run_checkpoint(&mut runtime, quantum)
                    } else {
                        run(&mut runtime, quantum)
                    }
                    let actual = (
                        runtime.results().unwrap(),
                        state(&runtime),
                        runtime.fuel_consumed(),
                        trace.borrow().clone(),
                    );
                    if let Some(expected) = &expected {
                        assert_eq!(&actual, expected)
                    } else {
                        expected = Some(actual)
                    }
                }
            }
        }
    }
}

#[test]
fn restore_rejects_tampered_hook_states_and_boundaries() {
    use crate::snapshot::{BoundaryImage, HookTargetImage};
    let mut runtime = boot(
        "debug.sethook(function() local a=1 end,'crl',3) local function f(a) return a,nil end return f(7)",
        crate::GcMode::Incremental,
    );
    let mut journal = Journal::new();
    for _ in 0..1000 {
        runtime.run(1, &mut journal).unwrap();
        if runtime
            .to_image()
            .unwrap()
            .threads
            .iter()
            .any(|t| t.hook.as_ref().is_some_and(|h| !h.allow_hook))
        {
            break;
        }
    }
    let image = runtime.to_image().unwrap();
    let at = image.threads.iter().position(|t| t.hook.is_some()).unwrap();
    assert!(!image.threads[at].hook.as_ref().unwrap().allow_hook);
    let registry = HostRegistry::proof();
    assert!(Runtime::from_snapshot(&snapshot::encode(&image).unwrap(), &registry, 1).is_ok());
    type Edit = fn(&mut crate::snapshot::ThreadImage);
    let edits: &[Edit] = &[
        |t| t.hook.as_mut().unwrap().mask |= 128,
        |t| t.hook.as_mut().unwrap().remaining_count = 0,
        |t| t.hook.as_mut().unwrap().remaining_count = 4,
        |t| t.hook.as_mut().unwrap().base_count = 0,
        |t| t.hook.as_mut().unwrap().allow_hook = true,
        |t| t.hook.as_mut().unwrap().transfer = None,
        |t| t.hook.as_mut().unwrap().transfer = Some((999, 1, 1)),
        |t| t.hook.as_mut().unwrap().transfer = Some((0, 0, 1)),
        |t| t.hook.as_mut().unwrap().hook_yield = true,
        |t| t.hook.as_mut().unwrap().instruction = Some((0, 0, 3)),
        |t| {
            t.hook.as_mut().unwrap().target =
                HookTargetImage::Lua(crate::snapshot::EncValue::Integer(1))
        },
        |t| t.hook.as_mut().unwrap().names[0] = crate::snapshot::EncValue::Integer(1),
        |t| {
            let hook = t.hook.as_mut().unwrap();
            hook.names.swap(0, 1);
        },
        |t| t.hook.as_mut().unwrap().old_pc = Some((0, 1 << 24, None)),
        |t| t.charged_held = 0,
        |t| {
            t.frames
                .iter_mut()
                .find_map(|f| match f.boundary.as_mut() {
                    Some(BoundaryImage::Hook { target, .. }) => Some(target),
                    _ => None,
                })
                .map(|target| *target = 999)
                .unwrap();
        },
        |t| {
            t.frames
                .iter_mut()
                .find_map(|f| match f.boundary.as_mut() {
                    Some(BoundaryImage::Hook { saved_top, .. }) => Some(saved_top),
                    _ => None,
                })
                .map(|top| *top = u32::MAX)
                .unwrap();
        },
    ];
    for (i, edit) in edits.iter().enumerate() {
        let mut corrupt = image.clone();
        edit(&mut corrupt.threads[at]);
        let bytes = snapshot::encode(&corrupt).unwrap();
        assert!(
            Runtime::from_snapshot(&bytes, &registry, 1).is_err(),
            "hook mutation {i}"
        );
    }
    let mut pending = boot(
        "debug.sethook(function() end,'crl',1) local a=3 return a",
        crate::GcMode::Incremental,
    );
    for _ in 0..1000 {
        pending.run(1, &mut journal).unwrap();
        if pending
            .to_image()
            .unwrap()
            .threads
            .iter()
            .any(|t| t.hook.as_ref().is_some_and(|h| h.pending.is_some()))
        {
            break;
        }
    }
    let mut pending = pending.to_image().unwrap();
    let t = pending
        .threads
        .iter_mut()
        .find(|t| t.hook.as_ref().is_some_and(|h| h.pending.is_some()))
        .unwrap();
    t.hook.as_mut().unwrap().pending.as_mut().unwrap().frame = u32::MAX;
    assert!(Runtime::from_snapshot(&snapshot::encode(&pending).unwrap(), &registry, 1).is_err());
}

#[test]
fn restore_rejects_forged_hook_continuations() {
    use crate::runtime::hooks::AfterHook;
    let mut runtime = boot(
        "debug.sethook(function() end,'crl',1) local function f() local x=3 return x end return f()",
        crate::GcMode::Incremental,
    );
    let registry = HostRegistry::proof();
    let mut journal = Journal::new();
    let mut checked = [false; 3];
    for _ in 0..1000 {
        if !matches!(
            runtime.run(1, &mut journal).unwrap(),
            StepOutcome::Paused(_)
        ) {
            break;
        }
        let image = runtime.to_image().unwrap();
        let Some(at) = image
            .threads
            .iter()
            .position(|t| t.hook.as_ref().is_some_and(|h| h.pending.is_some()))
        else {
            continue;
        };
        let event = image.threads[at].hook.as_ref().unwrap().pending.unwrap();
        let mut edits = Vec::new();
        {
            let mut bad = image.clone();
            bad.threads[at].hook.as_mut().unwrap().after = AfterHook::Return {
                src: 0,
                produced: 0,
            };
            edits.push((0, bad));
        }
        if event.frame > 0 {
            let mut bad = image.clone();
            bad.threads[at]
                .hook
                .as_mut()
                .unwrap()
                .pending
                .as_mut()
                .unwrap()
                .frame = 0;
            edits.push((1, bad));
        }
        {
            let mut bad = image.clone();
            let pending = bad.threads[at]
                .hook
                .as_mut()
                .unwrap()
                .pending
                .as_mut()
                .unwrap();
            pending.event = Event::Line;
            pending.line = Some(u32::MAX);
            edits.push((2, bad));
        }
        for (i, bad) in edits {
            assert!(
                Runtime::from_snapshot(&snapshot::encode(&bad).unwrap(), &registry, 1).is_err(),
                "forged hook continuation {i}"
            );
            checked[i] = true;
        }
        if checked.iter().all(|v| *v) {
            break;
        }
    }
    assert_eq!(checked, [true; 3]);
}

#[test]
fn lua_hook_host_wait_restores_suppression_and_transfer() {
    let source = "local n=0 local once=true debug.sethook(function(e) n=n+1 if once then once=false park() end end,'crl',4) local function f() return 9 end f() debug.sethook() return n";
    let mut expected = None;
    for checkpoint in [false, true] {
        let mut runtime = boot(source, crate::GcMode::Incremental);
        runtime.set_global_native("park", "park").unwrap();
        let mut journal = Journal::new();
        let mut waited = false;
        for _ in 0..10_000 {
            if checkpoint {
                runtime =
                    Runtime::from_snapshot(&runtime.snapshot().unwrap(), &HostRegistry::proof(), 1)
                        .unwrap();
            }
            match runtime.run(1, &mut journal).unwrap() {
                StepOutcome::Paused(_) => {}
                StepOutcome::Waiting(key) => {
                    waited = true;
                    let state = runtime
                        .heap()
                        .hooks
                        .get(
                            runtime
                                .heap()
                                .threads
                                .get(runtime.heap().active.unwrap())
                                .unwrap()
                                .id,
                        )
                        .unwrap();
                    assert!(!state.allow_hook && state.transfer.is_some());
                    runtime.complete_wait(key, 0).unwrap();
                }
                StepOutcome::Completed => break,
                other => panic!("{other:?}"),
            }
        }
        assert!(waited);
        let actual = (
            runtime.results().unwrap(),
            state(&runtime),
            runtime.fuel_consumed(),
        );
        if let Some(expected) = &expected {
            assert_eq!(&actual, expected);
        } else {
            expected = Some(actual);
        }
    }
}

#[test]
fn snapshot_sizes_cover_hook_states() {
    let source = "local a=0 for i=1,3 do a=a+i end return a";
    let plain = boot(source, crate::GcMode::Incremental);
    eprintln!("snapshot-size no-hook {}", plain.snapshot().unwrap().len());
    let mut host = plain;
    // Stable registration is required even before a first delivery.
    let mut registry = HostRegistry::proof();
    registry.register_hook("size.hook", |_| Ok(crate::HookAction::Continue));
    host = Runtime::from_snapshot(&host.snapshot().unwrap(), &registry, 1).unwrap();
    host.set_hook(None, "size.hook", crate::HookMask::LINE, 2)
        .unwrap();
    eprintln!("snapshot-size host-hook {}", host.snapshot().unwrap().len());
    let mut lua = boot(
        "debug.sethook(function() local n=1 end,'crl',2) local a=3 return a",
        crate::GcMode::Incremental,
    );
    let mut journal = Journal::new();
    let mut seen_lua = false;
    let mut seen_pending = false;
    for _ in 0..1000 {
        lua.run(1, &mut journal).unwrap();
        let image = lua.to_image().unwrap();
        if image.threads.iter().any(|t| {
            t.hook
                .as_ref()
                .is_some_and(|h| h.allow_hook && h.pending.is_none())
        }) && !seen_lua
        {
            eprintln!("snapshot-size lua-hook {}", lua.snapshot().unwrap().len());
            seen_lua = true;
        }
        if image
            .threads
            .iter()
            .any(|t| t.hook.as_ref().is_some_and(|h| h.pending.is_some()))
        {
            eprintln!(
                "snapshot-size pending-event {}",
                lua.snapshot().unwrap().len()
            );
            seen_pending = true;
        }
        if seen_lua && seen_pending {
            break;
        }
    }
    assert!(seen_lua && seen_pending);
    let chunk = crate::compile(b"local co=coroutine.create(function() install() return 7 end) coroutine.resume(co) return co").unwrap();
    registry.register_hook("size.yield", |_| Ok(crate::HookAction::Yield));
    registry.typed("install", crate::NativePolicy::VmLocal, |cx, ()| {
        cx.set_hook(None, "size.yield", crate::HookMask::NONE, 1)
    });
    let mut yielded = Runtime::boot(Config::default(), registry, &chunk.proto, false).unwrap();
    yielded.install_standard().unwrap();
    yielded.install_debug().unwrap();
    yielded.set_global_native("install", "install").unwrap();
    run(&mut yielded, 1);
    assert!(
        yielded
            .to_image()
            .unwrap()
            .threads
            .iter()
            .any(|t| t.hook.as_ref().is_some_and(|h| h.hook_yield))
    );
    eprintln!(
        "snapshot-size suspended-external-yield {}",
        yielded.snapshot().unwrap().len()
    );
}

#[test]
fn multiline_branch_and_operand_lines_survive_checkpoints() {
    for (source, expected) in [
        ("if\nmath.sin(1)\nthen\n a=1\nelse\n a=2\nend\n", "2,3,4,7"),
        ("local b={10}\na=b[1]\n+\nb[1]\nb=4\n", "1,3,4,3,4,5"),
    ] {
        let program = format!(
            "local trace={{}} local f=assert(load([=[{source}]=],'@victim.lua')) debug.sethook(function(e,l) if debug.getinfo(2,'S').source=='@victim.lua' then trace[#trace+1]=l end end,'l') f() debug.sethook() return table.concat(trace,',')"
        );
        for gc in [crate::GcMode::Incremental, crate::GcMode::Generational] {
            for quantum in [1, 2, 3, 7] {
                let mut runtime = boot(&program, gc);
                run_checkpoint(&mut runtime, quantum);
                assert_eq!(
                    runtime.results().unwrap(),
                    vec![HostValue::String(expected.as_bytes().to_vec())]
                );
            }
        }
    }
}

#[test]
fn debug_inspection_counts_a_hooked_native_activation_once() {
    let source = "local function f() local info=debug.getinfo(1,'Sl') assert(info.what=='Lua' and info.currentline>0) end debug.sethook(function() end,'cr') f() debug.sethook() return true";
    for quantum in [1, 2, 3, 7] {
        let mut runtime = boot(source, crate::GcMode::Incremental);
        run_checkpoint(&mut runtime, quantum);
        assert_eq!(runtime.results().unwrap(), vec![HostValue::Boolean(true)]);
    }
}

#[test]
fn failed_hook_coroutines_close_under_checkpoints() {
    for clear in [true, false] {
        let removal = if clear { "debug.sethook(co)" } else { "" };
        let source = format!(
            "local closed=0 local once=true local co=coroutine.create(function() local o <close> = setmetatable({{}}, {{__close=function() closed=closed+1 end}}) debug.sethook(function() local h <close> = setmetatable({{}}, {{__close=function() closed=closed+1 end}}) if once then once=false error('hook',0) end end,'crl',1) return 1 end) local ok,e=coroutine.resume(co) {removal} local ok2,e2=coroutine.close(co) return ok,ok2,e2==e,closed"
        );
        let mut reference = boot(&source, crate::GcMode::Incremental);
        run(&mut reference, u64::MAX);
        let expected = reference.results().unwrap();
        assert!(
            matches!(expected.as_slice(),[HostValue::Boolean(false),HostValue::Boolean(false),HostValue::Boolean(true),HostValue::Integer(n)] if *n>0)
        );
        for quantum in [1, 2, 3, 7] {
            let mut runtime = boot(&source, crate::GcMode::Incremental);
            run_checkpoint(&mut runtime, quantum);
            assert_eq!(runtime.results().unwrap(), expected);
            assert_eq!(runtime.fuel_consumed(), reference.fuel_consumed());
        }
    }
}
