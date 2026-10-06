//! Installed OS surfaces that the frozen byte corpus cannot exercise.
#![allow(clippy::arc_with_non_send_sync)] // Capability objects intentionally need not be Send/Sync.
use super::*;
use crate::hostcaps::testing::{FixedClock, MemoryEnvironment, MockProcess, PendingHost};
use crate::{CallOutcome, CapabilityValue, ExitStatus, Host, HostCapabilities, Libraries, Value};
use std::sync::Arc;

fn runtime(source: &str, caps: HostCapabilities) -> Runtime {
    let mut runtime = Runtime::builder()
        .libraries(Libraries::ALL)
        .capabilities(caps)
        .build()
        .unwrap();
    runtime
        .load_main(&crate::compile(source.as_bytes()).unwrap())
        .unwrap();
    runtime
}
fn restored(runtime: &Runtime, caps: HostCapabilities) -> Runtime {
    let mut registry = HostRegistry::new();
    crate::register_standard(&mut registry);
    crate::register_debug(&mut registry);
    let bytes = runtime.snapshot().unwrap();
    let restored = Runtime::restore(&bytes, &Host::new(registry).capabilities(caps)).unwrap();
    assert_eq!(bytes, restored.snapshot().unwrap());
    restored
}
#[test]
fn installed_exit_and_arg_public_api() {
    for (args, status) in [
        ("", ExitStatus::Success),
        ("true", ExitStatus::Success),
        ("false", ExitStatus::Failure),
        ("7", ExitStatus::Code(7)),
    ] {
        for close in [false, true] {
            let args = if args.is_empty() { "nil" } else { args };
            let source = format!(
                "count=0; local gc=setmetatable({{}},{{__gc=function() count=count+10 end}}); local c <close> = setmetatable({{}},{{__close=function() count=count+1 end}}); pcall(function() os.exit({args},{close}) end); error('caught')"
            );
            let mut r = runtime(&source, HostCapabilities::sandbox());
            let mut j = Journal::new();
            loop {
                let result = r.run(1, &mut j).unwrap();
                if matches!(result, StepOutcome::Paused(_)) {
                    r = restored(&r, HostCapabilities::sandbox());
                } else {
                    assert_eq!(result, StepOutcome::ExitRequested { status, close });
                    break;
                }
            }
            assert_eq!(
                r.globals().raw_get::<_, i64>(&mut r, "count").unwrap(),
                if close { 11 } else { 0 }
            );
        }
    }
    let mut r = runtime(
        "function quit() os.exit(false) end",
        HostCapabilities::sandbox(),
    );
    let mut j = Journal::new();
    assert_eq!(
        r.run_until_terminal(u64::MAX, &mut j).unwrap(),
        StepOutcome::Completed
    );
    let f = r
        .globals()
        .raw_get::<_, crate::Function>(&mut r, "quit")
        .unwrap();
    assert!(matches!(
        r.call::<()>(&f, (), &mut j, u64::MAX).unwrap(),
        CallOutcome::ExitRequested {
            status: ExitStatus::Failure,
            close: false
        }
    ));
    let mut r = Runtime::builder()
        .libraries(Libraries::ALL)
        .build()
        .unwrap();
    assert!(matches!(
        r.globals().raw_get::<_, Value>(&mut r, "arg").unwrap(),
        Value::Nil
    ));
    r.install_arg(
        b"script\0.lua",
        &[b"a".as_slice(), b"", b"\xff"],
        &[b"engine".as_slice(), b"option"],
    )
    .unwrap();
    r.load_main(&crate::compile(b"assert(arg[-2]=='engine' and arg[-1]=='option' and arg[0]=='script\\0.lua' and arg[1]=='a' and arg[2]=='' and arg[3]=='\\255' and arg[4]==nil)").unwrap()).unwrap();
    assert_eq!(
        r.run_until_terminal(u64::MAX, &mut Journal::new()).unwrap(),
        StepOutcome::Completed
    );
}
#[test]
fn os_capability_waits_replay_and_hooks() {
    let pending = Arc::new(PendingHost::new());
    let caps = HostCapabilities::sandbox()
        .clock(pending.clone())
        .civil(pending.clone())
        .environment(pending.clone())
        .filesystem(pending.clone())
        .process(pending.clone());
    let source = "counts={}; debug.sethook(function(e) local f=debug.getinfo(2,'f').func; if f==os.date or f==os.time then local k=(f==os.date and 'date' or 'time')..e; counts[k]=(counts[k] or 0)+1 end end,'cr'); local n=os.time(); assert(n==123); assert(os.clock()==1.25); assert(os.getenv('A')=='old'); assert(os.remove('x')); assert(os.rename('a','b')); assert(os.tmpname()=='tmp'); assert(os.execute()); local ok,kind,code=os.execute('cmd'); assert(ok==nil and kind=='signal' and code==9); assert(os.date('%z %Z',0)=='+0100 ZONE'); local t={year=1970,month=1,day=1,hour=0}; assert(os.time(t)==0 and t.hour==1); debug.sethook(); assert(counts.datecall==1 and counts.datereturn==1 and counts.timecall==2 and counts.timereturn==2)";
    let mut r = runtime(source, caps.clone());
    let mut j = Journal::new();
    let results = [
        CapabilityValue::Integer(123),
        CapabilityValue::Number(1.25),
        CapabilityValue::OptionalBytes(Some(b"old".to_vec())),
        CapabilityValue::Unit,
        CapabilityValue::Unit,
        CapabilityValue::Bytes(b"tmp".to_vec()),
        CapabilityValue::Boolean(true),
        CapabilityValue::Status(crate::ProcessStatus::Signal(9)),
        CapabilityValue::Offset(crate::CivilOffset {
            seconds: 3600,
            isdst: false,
        }),
        CapabilityValue::Bytes(b"ZONE".to_vec()),
        CapabilityValue::Integer(0),
        CapabilityValue::Offset(crate::CivilOffset {
            seconds: 3600,
            isdst: false,
        }),
    ];
    for result in results {
        let StepOutcome::Waiting(key) = r.run_until_terminal(u64::MAX, &mut j).unwrap() else {
            panic!("expected wait")
        };
        r = restored(&r, caps.clone());
        r.complete_capability(key, Ok(result)).unwrap();
        r = restored(&r, caps.clone());
    }
    assert_eq!(
        r.run_until_terminal(u64::MAX, &mut j).unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(pending.calls().len(), 12);
    assert_eq!(
        j.entries()
            .iter()
            .filter(|entry| entry.request.is_some())
            .count(),
        12
    );
    let env = Arc::new(MemoryEnvironment::new());
    env.set(b"A".to_vec(), b"old".to_vec());
    let process = Arc::new(MockProcess::new(crate::ProcessStatus::Exit(0)));
    let caps = HostCapabilities::sandbox()
        .environment(env.clone())
        .process(process.clone())
        .clock(Arc::new(FixedClock { now: 42, cpu: 0.5 }));
    let mut r = runtime(
        "a=os.getenv('A'); os.execute('cmd'); b=os.getenv('A')",
        caps.clone(),
    );
    let before = r.snapshot().unwrap();
    let mut j = Journal::new();
    assert_eq!(
        r.run_until_terminal(u64::MAX, &mut j).unwrap(),
        StepOutcome::Completed
    );
    env.set(b"A".to_vec(), b"new".to_vec());
    let mut registry = HostRegistry::new();
    crate::register_standard(&mut registry);
    crate::register_debug(&mut registry);
    let mut r = Runtime::restore(&before, &Host::new(registry).capabilities(caps)).unwrap();
    assert_eq!(
        r.run_until_terminal(u64::MAX, &mut j).unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(process.commands().len(), 1);
    assert_eq!(
        r.globals()
            .raw_get::<_, crate::LuaString>(&mut r, "b")
            .unwrap()
            .as_bytes(&r)
            .unwrap(),
        b"old"
    );
}
#[test]
fn os_table_metamethods_and_bounded_date() {
    let source = "local reads,writes={},{}; local t=setmetatable({}, {__index=function(_,k) reads[#reads+1]=k; if k=='year' then return 1970 elseif k=='month' or k=='day' then return 1 elseif k=='hour' then return 0 end end,__newindex=function(_,k,v) writes[#writes+1]=k; rawset(t or {},k,v) end}); assert(os.time(t)==0); assert(table.concat(reads,',')=='year,month,day,hour,min,sec,isdst'); assert(table.concat(writes,',')=='year,month,day,hour,min,sec,yday,wday,isdst'); assert(os.date('!'..string.rep('a',32)..'*t',0)==string.rep('a',32)..'*t'); assert(os.date('!'..string.rep('a',32)..'!%Y',0)==string.rep('a',32)..'!1970'); local ok,e=pcall(os.date,'!%\\255bad',0); assert(not ok and e:find('%\\255bad',1,true)); result=os.date('!'..string.rep('%Yabc',1000),0); assert(#result==7000); local ok,err=pcall(os.time,{year=1969,month=12,day=31,hour=23,min=59,sec=59}); assert(not ok)";
    let mut r = runtime(source, HostCapabilities::sandbox());
    let mut j = Journal::new();
    let mut pauses = 0;
    loop {
        match r.run(10, &mut j).unwrap() {
            StepOutcome::Paused(_) => {
                pauses += 1;
                r = restored(&r, HostCapabilities::sandbox());
            }
            StepOutcome::Completed => break,
            other => panic!("{other:?} {:?}", r.lua_error()),
        }
    }
    assert!(pauses > 100);
}

#[test]
fn os_filesystem_and_absent_authority() {
    let fs = Arc::new(
        crate::MemoryFilesystem::new(
            [(b"work".to_vec(), b"bytes".to_vec())],
            crate::MemoryOptions::default(),
        )
        .unwrap(),
    );
    let caps = HostCapabilities::sandbox().filesystem(fs.clone());
    let source = "assert(os.rename('work','renamed')); assert(os.remove('renamed')); local n,msg,code=os.remove('missing'); assert(n==nil and type(msg)=='string' and code==2); local path=os.tmpname(); assert(type(path)=='string'); saved=path; assert(os.remove(path)); local ok,err=pcall(os.time); assert(not ok and err:find('time source not available',1,true)); assert(os.getenv('X')==nil and os.execute()==false); local ok,msg,code=os.execute('cmd'); assert(ok==nil and type(msg)=='string' and code==13)";
    let mut r = runtime(source, caps.clone());
    let before = r.snapshot().unwrap();
    let mut j = Journal::new();
    assert_eq!(
        r.run_until_terminal(u64::MAX, &mut j).unwrap(),
        StepOutcome::Completed
    );
    assert!(fs.contents(b"work").is_none());
    let mut registry = HostRegistry::new();
    crate::register_standard(&mut registry);
    crate::register_debug(&mut registry);
    let mut replay = Runtime::restore(&before, &Host::new(registry).capabilities(caps)).unwrap();
    assert_eq!(
        replay.run_until_terminal(u64::MAX, &mut j).unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(
        r.globals()
            .raw_get::<_, crate::LuaString>(&mut r, "saved")
            .unwrap()
            .as_bytes(&r)
            .unwrap(),
        replay
            .globals()
            .raw_get::<_, crate::LuaString>(&mut replay, "saved")
            .unwrap()
            .as_bytes(&replay)
            .unwrap()
    );
}

#[test]
fn os_rejects_impossible_work_snapshots() {
    let mut r = runtime(
        "result=os.date('!'..string.rep('%Y',100),0)",
        HostCapabilities::sandbox(),
    );
    let mut j = Journal::new();
    loop {
        assert!(matches!(r.run(1, &mut j).unwrap(), StepOutcome::Paused(_)));
        let active = r.heap().active.unwrap();
        let work = r
            .heap_mut()
            .threads
            .get_mut(active)
            .unwrap()
            .frames
            .last_mut()
            .and_then(|frame| frame.boundary_mut());
        if let Some(crate::heap::Boundary::Builtin {
            task: crate::heap::Task::Lib(task),
            ..
        }) = work
            && let crate::library::Work::Os(work) = &mut task.work
            && !work.out.is_empty()
        {
            work.pos = u32::MAX;
            break;
        }
    }
    let bytes = r.snapshot().unwrap();
    let mut registry = HostRegistry::new();
    crate::register_standard(&mut registry);
    crate::register_debug(&mut registry);
    assert!(matches!(
        Runtime::restore(&bytes, &Host::new(registry)),
        Err(crate::Error::Vm(crate::VmError::Snapshot(
            crate::SnapshotError::InvalidStructure
        )))
    ));
}
