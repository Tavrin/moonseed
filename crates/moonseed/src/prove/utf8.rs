//! UTF-8 checkpoint, quota and builtin-hook proofs.
use super::*;
use crate::GcMode;
use crate::library::Work;
use crate::utf8lib::Utf8Work;

fn boot(source: &[u8], gc_mode: GcMode, quota: u64) -> Runtime {
    let chunk = crate::compile(source).unwrap();
    let mut runtime = Runtime::boot(
        Config {
            gc_mode,
            max_logical_heap: quota,
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
fn matrix(source: &[u8]) {
    for gc in [GcMode::Incremental, GcMode::Generational] {
        let mut expected = boot(source, gc, Config::default().max_logical_heap);
        let outcome = expected
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap();
        assert_eq!(
            outcome,
            StepOutcome::Completed,
            "{:?}",
            expected.lua_error()
        );
        for quantum in [1, 2, 3, 7] {
            let mut straight = boot(source, gc, Config::default().max_logical_heap);
            let mut restored = boot(source, gc, Config::default().max_logical_heap);
            let mut journals = [Journal::new(), Journal::new()];
            loop {
                let bytes = restored.snapshot().unwrap();
                restored = Runtime::from_snapshot(
                    &bytes,
                    &HostRegistry::proof(),
                    restored.effect_domain(),
                )
                .unwrap();
                let a = straight.run(quantum, &mut journals[0]).unwrap();
                let b = restored.run(quantum, &mut journals[1]).unwrap();
                assert_eq!(a, b);
                crate::gc::check_usage(straight.heap()).unwrap();
                crate::gc::check_usage(restored.heap()).unwrap();
                assert_eq!(
                    straight.memory().logical_bytes,
                    restored.memory().logical_bytes
                );
                // Hook debug-info tables can restore with a different slot layout;
                // compare their observable trace, fuel and exact heap charge below.
                if !source
                    .windows(b"debug.sethook".len())
                    .any(|s| s == b"debug.sethook")
                {
                    assert!(straight.snapshot().unwrap() == restored.snapshot().unwrap());
                }

                if !matches!(a, StepOutcome::Paused(_)) {
                    assert_eq!(a, outcome);
                    break;
                }
            }
            assert_eq!(pair_results(&restored), pair_results(&expected));
            assert_eq!(restored.fuel_consumed(), expected.fuel_consumed());
            assert_eq!(
                restored.memory().logical_bytes,
                expected.memory().logical_bytes
            );
        }
    }
}
#[test]
fn utf8_checkpoint_matrix() {
    for source in [
        &b"return utf8.len(string.rep('a',1024)),utf8.len(string.rep('a',512)..'\\255')"[..],
        b"local s=string.rep('a',600) local t={utf8.codepoint(s,1,#s)} return #t,t[1],t[600],pcall(utf8.codepoint,s..'\\255',1,601)",
        b"local t={} for i=1,600 do t[i]=0x7fffffff end local s=utf8.char(table.unpack(t)) return #s,utf8.len(s,1,-1,true)",
        b"local s=string.rep('a',9000) return utf8.offset(s,8999),utf8.offset(s,-8999),utf8.offset('a'..string.rep('\\128',9000),0,9001)",
        b"local n=0 for p,c in utf8.codes('a\\195\\169\\0') do n=n+p+c end return n",
        b"local f=utf8.codes('') return f('a'..string.rep('\\128',9000)..'b',1)",
    ] { matrix(source); }
}
#[test]
fn utf8_encoder_lexer_and_module_profile() {
    let mut runtime = crate::Runtime::builder()
        .libraries(crate::Libraries::UTF8 | crate::Libraries::BASE | crate::Libraries::PACKAGE)
        .build()
        .unwrap();
    runtime.load_main(&crate::compile(br"assert(require('utf8')==utf8) return utf8.char(0,0xd800,0x7fffffff)=='\u{0}\u{d800}\u{7fffffff}'").unwrap()).unwrap();
    assert_eq!(
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(
        runtime.results().unwrap(),
        vec![crate::HostValue::Boolean(true)]
    );
    let mut runtime = crate::Runtime::builder()
        .libraries(crate::Libraries::BASE)
        .build()
        .unwrap();
    runtime
        .load_main(&crate::compile(b"return utf8==nil").unwrap())
        .unwrap();
    runtime
        .run_until_terminal(u64::MAX, &mut Journal::new())
        .unwrap();
    assert_eq!(
        runtime.results().unwrap(),
        vec![crate::HostValue::Boolean(true)]
    );
}
#[test]
fn utf8_quota_and_result_preflight() {
    let source =
        b"local t={} for i=1,600 do t[i]=0x7fffffff end return pcall(utf8.char,table.unpack(t))";
    let mut successful = boot(
        source,
        GcMode::Incremental,
        Config::default().max_logical_heap,
    );
    let mut peak = 0;
    let mut journal = Journal::new();
    loop {
        let outcome = successful.run(1, &mut journal).unwrap();
        crate::gc::check_usage(successful.heap()).unwrap();
        peak = peak.max(successful.memory().logical_bytes);
        if !matches!(outcome, StepOutcome::Paused(_)) {
            assert_eq!(outcome, StepOutcome::Completed);
            break;
        }
    }
    let mut saw_refusal = false;
    let mut saw_success = false;
    for gc in [GcMode::Incremental, GcMode::Generational] {
        for quota in [
            peak / 2,
            peak * 3 / 4,
            peak.saturating_sub(8192),
            peak,
            peak + 4096,
        ] {
            let mut runtime = boot(source, gc, quota);
            loop {
                let outcome = runtime.run(1, &mut Journal::new()).unwrap();
                crate::gc::check_usage(runtime.heap()).unwrap();
                let bytes = runtime.snapshot().unwrap();
                runtime =
                    Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain())
                        .unwrap();
                if !matches!(outcome, StepOutcome::Paused(_)) {
                    if outcome == StepOutcome::Completed {
                        match runtime.results().unwrap().first() {
                            Some(crate::HostValue::Boolean(true)) => saw_success = true,
                            _ => saw_refusal = true,
                        }
                    } else {
                        saw_refusal = true;
                    }
                    break;
                }
            }
        }
    }
    assert!(
        saw_refusal && saw_success,
        "peak={peak} refusal={saw_refusal} success={saw_success}"
    );
    let chunk = crate::compile(source).unwrap();
    let mut limited = Runtime::boot(
        Config {
            max_string_bytes: 1024,
            ..Config::default()
        },
        HostRegistry::proof(),
        &chunk.proto,
        false,
    )
    .unwrap();
    limited.install_standard().unwrap();
    assert_eq!(
        limited
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Completed
    );
    assert!(matches!(
        limited.results().unwrap().first(),
        Some(crate::HostValue::Boolean(false))
    ));
    crate::gc::check_usage(limited.heap()).unwrap();
    let source = b"return pcall(utf8.codepoint,string.rep('a',2000),1,2000)";
    let chunk = crate::compile(source).unwrap();
    let mut runtime = Runtime::boot(
        Config {
            max_stack_slots: 1024,
            ..Config::default()
        },
        HostRegistry::proof(),
        &chunk.proto,
        false,
    )
    .unwrap();
    runtime.install_standard().unwrap();
    assert_eq!(
        runtime
            .run_until_terminal(u64::MAX, &mut Journal::new())
            .unwrap(),
        StepOutcome::Completed
    );
    let results = runtime.results().unwrap();
    assert!(matches!(
        results.first(),
        Some(crate::HostValue::Boolean(false))
    ));
    assert!(
        matches!(results.get(1),Some(crate::HostValue::String(bytes)) if bytes.ends_with(b"stack overflow (string slice too long)"))
    );
}
#[test]
fn utf8_snapshot_shapes_reject_impossible_state() {
    assert!(
        !Utf8Work::Char {
            next: 1,
            out: vec![0; 7]
        }
        .fits(2, None)
    );
    assert!(
        !Utf8Work::Scan {
            pos: 9,
            end: 2,
            count: 0,
            strict: true,
            points: false
        }
        .fits(1, Some(3))
    );
    assert!(
        !Utf8Work::Offset {
            pos: 0,
            remaining: i64::MIN,
            direction: 1,
            seeking: false
        }
        .fits(2, Some(3))
    );
    let work = Work::Utf8(Box::new(Utf8Work::Scan {
        pos: 1,
        end: 3,
        count: 1,
        strict: true,
        points: true,
    }));
    assert_eq!(work.scratch(), 1);
}
#[test]
fn utf8_hook_transfer_and_internal_steps() {
    matrix(br#"
        local events={}
        local functions={[utf8.char]=true,[utf8.len]=true,[utf8.codepoint]=true,[utf8.offset]=true,[utf8.codes]=true}
        local f=utf8.codes('') functions[f]=true
        debug.sethook(function(e)
          local d=debug.getinfo(2,'fr')
          if functions[d.func] then events[#events+1]=e..':'..d.ftransfer..':'..d.ntransfer end
        end,'cr')
        local t={} for i=1,600 do t[i]=65 end
        local s=utf8.char(table.unpack(t))
        local n=utf8.len(s)
        local a={utf8.codepoint(s,1,600)}
        local p=utf8.offset(s,590)
        local it,state,control=utf8.codes('a')
        local x,y=it(state,control)
        local z=it(state,x)
        debug.sethook()
        assert(#events==14)
        assert(events[1]=='call:1:600' and events[2]=='return:601:1')
        assert(events[5]=='call:1:3' and events[6]=='return:4:600')
        assert(events[11]=='call:1:2' and events[12]=='return:3:2')
        assert(events[14]=='return:0:0')
        return n,#a,p,x,y,z,table.concat(events,',')
    "#);
}

#[test]
fn utf8_long_scans_keep_exact_counts_near_quota() {
    for source in [
        &b"local s=string.rep('a',8192) return utf8.len(s),utf8.offset(s,8192),utf8.offset(s,-8192)"[..],
        b"local s=string.rep('a',1000) local t={utf8.codepoint(s,1,#s)} return #t,t[1000]",
        b"local f=utf8.codes('') local s='a'..string.rep('\\128',8192)..'b' return f(s,1)",
    ] {
        let mut reference = boot(source, GcMode::Incremental, Config::default().max_logical_heap);
        let installed = reference.memory().logical_bytes;
        let mut peak = installed;
        loop {
            let outcome = reference.run(1, &mut Journal::new()).unwrap();
            crate::gc::check_usage(reference.heap()).unwrap();
            peak = peak.max(reference.memory().logical_bytes);
            if !matches!(outcome, StepOutcome::Paused(_)) {
                assert_eq!(outcome, StepOutcome::Completed);
                break;
            }
        }
        for gc in [GcMode::Incremental, GcMode::Generational] {
            for quota in [peak.saturating_sub(1024).max(installed + 256), peak, peak + 4096] {
                let mut runtime = boot(source, gc, quota);
                loop {
                    let outcome = runtime.run(1, &mut Journal::new()).unwrap();
                    crate::gc::check_usage(runtime.heap()).unwrap();
                    let bytes = runtime.snapshot().unwrap();
                    runtime = Runtime::from_snapshot(&bytes, &HostRegistry::proof(), runtime.effect_domain()).unwrap();
                    if !matches!(outcome, StepOutcome::Paused(_)) {
                        if outcome == StepOutcome::Completed {
                            assert_eq!(pair_results(&runtime), pair_results(&reference));
                        } else {
                            assert!(matches!(outcome, StepOutcome::LuaError(_)));
                        }
                        break;
                    }
                }
            }
        }
    }
}

#[test]
fn utf8_restore_rejects_tampered_work() {
    use crate::snapshot::{BoundaryImage, Image};
    fn paused(source: &[u8]) -> (Runtime, Image, usize, usize) {
        let mut runtime = boot(
            source,
            GcMode::Incremental,
            Config::default().max_logical_heap,
        );
        for _ in 0..10000 {
            runtime.run(1, &mut Journal::new()).unwrap();
            let image = runtime.to_image().unwrap();
            for (t, thread) in image.threads.iter().enumerate() {
                for (f, frame) in thread.frames.iter().enumerate() {
                    if matches!(&frame.boundary, Some(BoundaryImage::Builtin {
                        task: crate::heap::Task::Lib(task), ..
                    }) if matches!(task.work, Work::Utf8(_)))
                    {
                        return (runtime, image, t, f);
                    }
                }
            }
        }
        panic!("no UTF-8 work boundary");
    }
    let mut accepted = Vec::new();
    for (source, edits) in [
        (
            &b"return utf8.len(string.rep('a',1024))"[..],
            &[
                "missing subject",
                "past string",
                "count past position",
                "scan past end",
                "callback wait",
            ][..],
        ),
        (
            &b"return utf8.codepoint(string.rep('a',1024),1,1024)"[..],
            &["missing scratch"][..],
        ),
        (
            &b"return utf8.offset(string.rep('a',9000),8999)"[..],
            &["exhausted seek", "past string"][..],
        ),
        (
            &b"local f=utf8.codes('') return f('a'..string.rep('\\128',9000)..'b',1)"[..],
            &["missing subject", "past string"][..],
        ),
        (
            &b"local t={} for i=1,600 do t[i]=65 end return utf8.char(table.unpack(t))"[..],
            &["char counter", "short buffer", "uncharged buffer"][..],
        ),
    ] {
        let (runtime, image, t, f) = paused(source);
        assert!(
            Runtime::from_snapshot(
                &snapshot::encode(&image).unwrap(),
                &HostRegistry::proof(),
                runtime.effect_domain()
            )
            .is_ok()
        );
        for edit in edits {
            let mut bad = image.clone();
            let thread = &mut bad.threads[t];
            let Some(BoundaryImage::Builtin {
                passed,
                task: crate::heap::Task::Lib(task),
                ..
            }) = &mut thread.frames[f].boundary
            else {
                panic!()
            };
            let Work::Utf8(work) = &mut task.work else {
                panic!()
            };
            match (*edit, work.as_mut()) {
                ("missing subject", _) => *passed = 0,
                (
                    "past string",
                    Utf8Work::Scan { pos, .. }
                    | Utf8Work::Offset { pos, .. }
                    | Utf8Work::Iterate { pos, .. },
                ) => *pos = u32::MAX,
                ("count past position", Utf8Work::Scan { count, .. }) => *count = u32::MAX,
                ("scan past end", Utf8Work::Scan { end, .. }) => *end = 0,
                ("missing scratch", Utf8Work::Scan { pos, count, .. }) => {
                    *pos = 512;
                    *count = 512;
                }
                ("callback wait", _) => task.wait = crate::library::Wait::Set,
                (
                    "exhausted seek",
                    Utf8Work::Offset {
                        remaining, seeking, ..
                    },
                ) => {
                    *remaining = 0;
                    *seeking = true;
                }
                ("char counter", Utf8Work::Char { next, .. }) => *next = u32::MAX,
                ("short buffer", Utf8Work::Char { out, .. }) => out.clear(),
                ("uncharged buffer", _) => thread.charged_held = 0,
                _ => panic!("unknown edit"),
            }
            if Runtime::from_snapshot(
                &snapshot::encode(&bad).unwrap(),
                &HostRegistry::proof(),
                runtime.effect_domain(),
            )
            .is_ok()
            {
                accepted.push(*edit);
            }
        }
    }
    assert!(accepted.is_empty(), "accepted corrupt states: {accepted:?}");
}
