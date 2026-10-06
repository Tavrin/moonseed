#![allow(clippy::arc_with_non_send_sync)] // Capability traits intentionally share Arc without Send/Sync.
use super::*;
use crate::hostcaps::Completion;
use crate::{
    CapabilityValue, Filesystem, Host, HostCapabilities, Libraries, MemoryFilesystem,
    MemoryOptions, PendingToken, SnapshotError,
};
use std::cell::{Cell, RefCell};
use std::sync::Arc;

struct CountFs {
    fs: MemoryFilesystem,
    writes: Cell<usize>,
    closes: Cell<usize>,
    pending: bool,
    pending_close: Cell<Option<ResourceId>>,
    answer: RefCell<Option<Result<CapabilityValue, HostIoError>>>,
}
impl CountFs {
    fn new(data: Vec<u8>, pending: bool) -> Self {
        Self {
            fs: MemoryFilesystem::new([(b"f".to_vec(), data)], MemoryOptions::default()).unwrap(),
            writes: Cell::new(0),
            closes: Cell::new(0),
            pending,
            pending_close: Cell::new(None),
            answer: RefCell::new(None),
        }
    }
    fn done<T>(
        &self,
        result: Completion<T>,
        encode: impl FnOnce(T) -> CapabilityValue,
    ) -> Completion<T> {
        if !self.pending {
            return result;
        }
        let Completion::Ready(result) = result else {
            panic!("VFS pending");
        };
        *self.answer.borrow_mut() = Some(result.map(encode));
        Completion::Pending(PendingToken(17))
    }
}
impl Filesystem for CountFs {
    fn open(&self, path: &[u8], mode: OpenMode) -> Completion<ResourceId> {
        self.done(self.fs.open(path, mode), Answer::Resource)
    }
    fn read_at(&self, id: ResourceId, offset: u64, max: usize) -> Completion<Vec<u8>> {
        self.done(self.fs.read_at(id, offset, max), Answer::Bytes)
    }
    fn write_at(&self, id: ResourceId, offset: u64, bytes: &[u8]) -> Completion<usize> {
        self.writes.set(self.writes.get() + 1);
        self.done(self.fs.write_at(id, offset, bytes), |n| {
            Answer::Unsigned(n as u64)
        })
    }
    fn size(&self, id: ResourceId) -> Completion<u64> {
        self.done(self.fs.size(id), Answer::Unsigned)
    }
    fn flush(&self, id: ResourceId) -> Completion<()> {
        self.done(self.fs.flush(id), |_| Answer::Unit)
    }
    fn temp_file(&self) -> Completion<ResourceId> {
        self.done(self.fs.temp_file(), Answer::Resource)
    }
    fn close(&self, id: ResourceId) -> Completion<()> {
        self.closes.set(self.closes.get() + 1);
        if self.pending {
            self.pending_close.set(Some(id));
            *self.answer.borrow_mut() = Some(Ok(Answer::Unit));
            Completion::Pending(PendingToken(17))
        } else {
            self.fs.close(id)
        }
    }
    fn handle_policy(&self) -> HandlePolicy {
        HandlePolicy::Rebind
    }
    fn rebind(&self, id: ResourceId) -> Result<(), HostIoError> {
        self.fs.rebind(id)
    }
}
fn host(caps: HostCapabilities) -> Host {
    let mut r = HostRegistry::new();
    crate::register_standard(&mut r);
    crate::register_debug(&mut r);
    crate::register_io(&mut r);
    Host::new(r).capabilities(caps)
}
fn runtime(source: &[u8], caps: HostCapabilities) -> Runtime {
    let mut r = Runtime::builder()
        .config(Config {
            fuel_limit: None,
            ..Default::default()
        })
        .libraries(Libraries::ALL)
        .capabilities(caps)
        .build()
        .unwrap();
    r.load_main(&crate::compile(source).unwrap()).unwrap();
    r
}
fn run(r: &mut Runtime, j: &mut Journal) {
    assert_eq!(
        r.run_until_terminal(1000, j).unwrap(),
        StepOutcome::Completed
    );
}
#[test]
fn checkpoint_large_io_seek_lines_and_runtime_close() {
    let fs = Arc::new(CountFs::new(b"12\nsecond\n".to_vec(), false));
    let caps = HostCapabilities::sandbox().filesystem(fs.clone());
    let h = host(caps.clone());
    let mut r = runtime(
        br#"
        local n=assert(io.tmpfile()); n:write(string.rep('9',201)); n:seek('set')
        assert(n:read('n')==nil); assert(n:read('l')=='9'); assert(n:close())
        local f=assert(io.open('f','r+'))
        assert(f:read('n')==12); assert(f:read('l')=='')
        assert(f:read('l')=='second'); assert(f:seek('set')==0)
        local it=f:lines('L'); assert(it()=='12\n'); assert(it()=='second\n')
        f:seek('set'); local s=string.rep('x',1048576); assert(f:write(s)==f)
        assert(f:flush()); f:seek('set'); assert(f:read('a')==s)
        f:seek('end',-3); assert(f:read(3)=='xxx')
        file=f
    "#,
        caps,
    );
    let mut j = Journal::new();
    let mut checkpoints = 0;
    loop {
        let outcome = r.run(1, &mut j).unwrap();
        let bytes = r.snapshot().unwrap();
        r = Runtime::restore(&bytes, &h).unwrap();
        checkpoints += 1;
        match outcome {
            StepOutcome::Completed => break,
            StepOutcome::Paused(_) => {}
            other => panic!("{other:?}: {:?}", r.lua_error()),
        }
        assert!(checkpoints < 10000);
    }
    assert!(checkpoints > 30);
    assert_eq!(fs.writes.get(), 17);
    assert_eq!(fs.fs.open_count(), 1);
    r.begin_close().unwrap();
    while matches!(r.run(1, &mut j).unwrap(), StepOutcome::Paused(_)) {
        r = Runtime::restore(&r.snapshot().unwrap(), &h).unwrap();
    }
    assert_eq!(fs.closes.get(), 2);
    assert_eq!(fs.fs.open_count(), 0);
}
#[test]
fn exactly_once_write_and_old_read_replay_against_changed_world() {
    let fs = Arc::new(CountFs::new(vec![b'o'; 200000], false));
    let caps = HostCapabilities::sandbox().filesystem(fs.clone());
    let h = host(caps.clone());
    let mut r = runtime(br#"f=assert(io.open('f','r+')); old=f:read('a'); f:seek('set'); f:write(string.rep('n',200000)); f:seek('set'); assert(f:read('a')==string.rep('n',200000))"#,caps);
    let mut j = Journal::new();
    // Stop immediately after open, before the journaled reads/writes.
    loop {
        r.run(1, &mut j).unwrap();
        if fs.fs.open_count() == 1 {
            break;
        }
    }
    let before = r.snapshot().unwrap();
    run(&mut r, &mut j);
    assert_eq!(fs.writes.get(), 4);
    let mut replay = Runtime::restore(&before, &h).unwrap();
    run(&mut replay, &mut j);
    assert_eq!(fs.writes.get(), 4);
    let globals = replay.globals();
    let old: crate::api::LuaString = globals.raw_get(&mut replay, "old").unwrap();
    assert_eq!(old.as_bytes(&replay).unwrap(), vec![b'o'; 200000]);
}
#[test]
fn generic_for_closes_exactly_once_and_iterator_restore_shape() {
    let fs = Arc::new(CountFs::new(b"one\ntwo\n".to_vec(), false));
    let caps = HostCapabilities::sandbox().filesystem(fs.clone());
    let h = host(caps.clone());
    let mut r=runtime(br#"for s in io.lines('f') do assert(s=='one'); break end; assert(io.type(io.open('f'))=='file')"#,caps);
    let mut j = Journal::new();
    loop {
        let o = r.run(1, &mut j).unwrap();
        r = Runtime::restore(&r.snapshot().unwrap(), &h).unwrap();
        if o == StepOutcome::Completed {
            break;
        }
    }
    assert_eq!(fs.closes.get(), 1);
    let mt = r
        .heap
        .registry
        .and_then(|t| {
            r.heap
                .table_get_view(t, crate::table::KeyView::string(b"FILE*"))
        })
        .unwrap();
    assert!(matches!(mt, Value::Table(_)));
    let native = match r.native_value("io.linesstep").unwrap() {
        Value::Native(n) => n,
        _ => panic!(),
    };
    let bad = r
        .alloc_native_closure(native, vec![Value::Nil], vec![0])
        .unwrap();
    assert!(!lines_fits(
        &r.heap,
        &r.heap.native_closures.get(bad).unwrap().values,
        &[0]
    ));
    assert!(Runtime::restore(&r.snapshot().unwrap(), &h).is_err());
}
#[test]
fn native_policy_refuses_open_but_closed_files_encode() {
    // Use a refusing VFS wrapper, avoiding ambient filesystem test authority.
    struct Refuse(MemoryFilesystem);
    impl Filesystem for Refuse {
        fn open(&self, p: &[u8], m: OpenMode) -> Completion<ResourceId> {
            self.0.open(p, m)
        }
        fn close(&self, id: ResourceId) -> Completion<()> {
            self.0.close(id)
        }
    }
    let fs = Arc::new(Refuse(
        MemoryFilesystem::new([(b"f".to_vec(), vec![])], Default::default()).unwrap(),
    ));
    let caps = HostCapabilities::sandbox().filesystem(fs);
    let h = host(caps.clone());
    let mut r = runtime(b"f=assert(io.open('f'))", caps);
    let mut j = Journal::new();
    run(&mut r, &mut j);
    assert!(matches!(
        r.snapshot(),
        Err(SnapshotError::NonPortableResource { .. })
    ));
    r.load_main(&crate::compile(b"f:close()").unwrap()).unwrap();
    run(&mut r, &mut j);
    Runtime::restore(&r.snapshot().unwrap(), &h).unwrap();
}

#[test]
fn pending_io_preserves_requests_results_fuel_and_roots() {
    let fs = Arc::new(CountFs::new(vec![b'x'; 100000], true));
    let caps = HostCapabilities::sandbox().filesystem(fs.clone());
    let h = host(caps.clone());
    let mut r=runtime(br#"collectgarbage('stop'); local f=assert(io.open('f','r+')); assert(#f:read('a')==100000); assert(f:seek('end')==100000); f:seek('set'); f:write(string.rep('q',100000)); assert(f:flush()); f:close()"#,caps);
    let mut j = Journal::new();
    let mut waits = 0;
    loop {
        match r.run(1, &mut j).unwrap() {
            StepOutcome::Waiting(key) => {
                let fuel = r.fuel_consumed();
                assert_eq!(r.run(100, &mut j).unwrap(), StepOutcome::Waiting(key));
                assert_eq!(r.fuel_consumed(), fuel);
                r = Runtime::restore(&r.snapshot().unwrap(), &h).unwrap();
                if let Some(id) = fs.pending_close.take() {
                    assert!(matches!(fs.fs.close(id), Completion::Ready(Ok(()))));
                }
                let answer = fs.answer.borrow_mut().take().unwrap();
                r.complete_capability(key, answer).unwrap();
                r = Runtime::restore(&r.snapshot().unwrap(), &h).unwrap();
                waits += 1;
            }
            StepOutcome::Paused(_) => {
                r = Runtime::restore(&r.snapshot().unwrap(), &h).unwrap();
            }
            StepOutcome::Completed => break,
            o => panic!("{o:?}"),
        }
    }
    assert_eq!(waits, 10);
    assert_eq!(fs.writes.get(), 3);
    let ready_fs = Arc::new(CountFs::new(vec![b'x'; 100000], false));
    let mut ready = runtime(br#"collectgarbage('stop'); local f=assert(io.open('f','r+')); assert(#f:read('a')==100000); assert(f:seek('end')==100000); f:seek('set'); f:write(string.rep('q',100000)); assert(f:flush()); f:close()"#,HostCapabilities::sandbox().filesystem(ready_fs));
    run(&mut ready, &mut Journal::new());
    assert_eq!(ready.fuel_consumed(), r.fuel_consumed());
    assert_eq!(fs.fs.contents(b"f").unwrap(), vec![b'q'; 100000]);
}

#[test]
fn io_hooks_and_write_failure_still_validate_late_arguments() {
    let fs = Arc::new(CountFs::new(vec![], false));
    let mut r = runtime(
        br#"
        local f=assert(io.open('f','w+')); local nr,nw=0,0
        local rf,wf=f.read,f.write
        debug.sethook(function(e)
            local fn=debug.getinfo(2,'f').func
            if e=='call' and fn==rf then nr=nr+1 end
            if e=='return' and fn==wf then nw=nw+1 end
        end,'cr')
        f:write(string.rep('z',100000)); f:seek('set'); assert(#f:read('a')==100000)
        debug.sethook(); assert(nr==1 and nw==1)
        local mt=getmetatable(f); debug.setmetatable(f,nil); assert(io.type(f)==nil)
        local ok,e=pcall(rf,f,0); assert(not ok and e:find('FILE* expected',1,true))
        debug.setmetatable(f,mt); assert(io.type(f)=='file'); f:close()
        local f <close> = assert(io.open('f','r'))
        local ok,e=pcall(function() f:write('x',nil) end)
        assert(not ok and e:find('string expected, got nil',1,true))
    "#,
        HostCapabilities::sandbox().filesystem(fs),
    );
    run(&mut r, &mut Journal::new());
}

#[test]
fn review_iterator_read_errors_raise_and_seek_rejects_extremes() {
    struct ReadError(MemoryFilesystem);
    impl Filesystem for ReadError {
        fn open(&self, p: &[u8], m: OpenMode) -> Completion<ResourceId> {
            self.0.open(p, m)
        }
        fn read_at(&self, _: ResourceId, _: u64, _: usize) -> Completion<Vec<u8>> {
            Completion::Ready(Err(HostIoError::new(
                HostIoErrorKind::Other,
                b"read failed".to_vec(),
            )))
        }
        fn close(&self, id: ResourceId) -> Completion<()> {
            self.0.close(id)
        }
        fn handle_policy(&self) -> HandlePolicy {
            HandlePolicy::Rebind
        }
        fn rebind(&self, id: ResourceId) -> Result<(), HostIoError> {
            self.0.rebind(id)
        }
    }
    let fs = Arc::new(ReadError(
        MemoryFilesystem::new([(b"f".to_vec(), vec![])], Default::default()).unwrap(),
    ));
    let mut r = runtime(
        br#"
        local f <close> = assert(io.open('f'))
        local a,e,c=f:read('l'); assert(a==nil and e=='read failed' and c==5)
        local ok,e=pcall(f:lines()); assert(not ok and e:find('read failed',1,true))
        local ok,e=pcall(function() for _ in io.lines('f') do error('unexpected row') end end)
        assert(not ok and e:find('read failed',1,true))
    "#,
        HostCapabilities::sandbox().filesystem(fs.clone()),
    );
    run(&mut r, &mut Journal::new());
    assert_eq!(fs.0.open_count(), 0);
    let mut r = runtime(
        br#"
        local f <close> = assert(io.open('f','r+'))
        assert(f:seek('set',2)==2)
        local p,e,c=f:seek('set',math.maxinteger); assert(p==nil and c==22)
        assert(f:seek()==2)
        local p,e,c=f:seek('cur',math.maxinteger); assert(p==nil and c==22)
        assert(f:seek()==2)
        assert(f:seek('set',0)==0)
    "#,
        HostCapabilities::sandbox().filesystem(Arc::new(CountFs::new(vec![], false))),
    );
    run(&mut r, &mut Journal::new());
}

#[test]
fn review_completed_open_enforces_resource_snapshot_policy() {
    struct OpenWait {
        fs: MemoryFilesystem,
        id: Cell<Option<ResourceId>>,
        refuse: bool,
        panic_policy: Cell<bool>,
    }
    impl Filesystem for OpenWait {
        fn open(&self, p: &[u8], m: OpenMode) -> Completion<ResourceId> {
            let Completion::Ready(Ok(id)) = self.fs.open(p, m) else {
                panic!()
            };
            self.id.set(Some(id));
            Completion::Pending(PendingToken(1))
        }
        fn handle_policy(&self) -> HandlePolicy {
            assert!(
                !self.panic_policy.get(),
                "policy must not repeat on completion"
            );
            if self.refuse {
                HandlePolicy::Refuse
            } else {
                HandlePolicy::Rebind
            }
        }
        fn rebind(&self, id: ResourceId) -> Result<(), HostIoError> {
            self.fs.rebind(id)
        }
        fn close(&self, id: ResourceId) -> Completion<()> {
            self.fs.close(id)
        }
    }
    for refuse in [true, false] {
        let fs = Arc::new(OpenWait {
            fs: MemoryFilesystem::new([(b"f".to_vec(), vec![])], Default::default()).unwrap(),
            id: Cell::new(None),
            refuse,
            panic_policy: Cell::new(false),
        });
        let caps = HostCapabilities::sandbox().filesystem(fs.clone());
        let h = host(caps.clone());
        let mut r = runtime(b"local f <close> = assert(io.open('f'))", caps);
        let mut j = Journal::new();
        let StepOutcome::Waiting(key) = r.run(1000, &mut j).unwrap() else {
            panic!()
        };
        let id = fs.id.get().unwrap();
        if refuse {
            assert!(matches!(
                r.snapshot(),
                Err(SnapshotError::NonPortableResource { .. })
            ));
        }
        r.complete_capability(key, Ok(Answer::Resource(id)))
            .unwrap();
        if refuse {
            assert!(matches!(
                r.snapshot(),
                Err(SnapshotError::NonPortableResource { .. })
            ));
            fs.panic_policy.set(true);
            run(&mut r, &mut j);
            fs.panic_policy.set(false);
        } else {
            let image = r.snapshot().unwrap();
            let mut restored = Runtime::restore(&image, &h).unwrap();
            fs.panic_policy.set(true);
            run(&mut restored, &mut j);
            fs.panic_policy.set(false);
            assert!(
                Runtime::restore(&image, &h).is_err(),
                "completed acquisition must rebind its live resource"
            );
        }
        assert_eq!(fs.fs.open_count(), 0);
    }
}

#[test]
fn review_policy_panics_become_errors_before_open_and_during_restore() {
    struct PolicyPanic {
        fs: MemoryFilesystem,
        panic: Cell<bool>,
    }
    impl Filesystem for PolicyPanic {
        fn open(&self, p: &[u8], m: OpenMode) -> Completion<ResourceId> {
            self.fs.open(p, m)
        }
        fn close(&self, id: ResourceId) -> Completion<()> {
            self.fs.close(id)
        }
        fn handle_policy(&self) -> HandlePolicy {
            assert!(!self.panic.get(), "private host detail");
            HandlePolicy::Rebind
        }
        fn rebind(&self, id: ResourceId) -> Result<(), HostIoError> {
            self.fs.rebind(id)
        }
    }
    let fs = Arc::new(PolicyPanic {
        fs: MemoryFilesystem::new([(b"f".to_vec(), vec![])], Default::default()).unwrap(),
        panic: Cell::new(true),
    });
    let caps = HostCapabilities::sandbox().filesystem(fs.clone());
    let mut r=runtime(br#"local f,e,c=io.open('f'); assert(f==nil and e:find('host capability panicked',1,true) and c==5)"#,caps.clone());
    run(&mut r, &mut Journal::new());
    assert_eq!(fs.fs.open_count(), 0);
    fs.panic.set(false);
    let mut r = runtime(b"f=assert(io.open('f'))", caps.clone());
    let mut j = Journal::new();
    run(&mut r, &mut j);
    let image = r.snapshot().unwrap();
    fs.panic.set(true);
    assert!(Runtime::restore(&image, &host(caps)).is_err());
    fs.panic.set(false);
    r.begin_close().unwrap();
    run(&mut r, &mut j);
    assert_eq!(fs.fs.open_count(), 0);
}

#[test]
fn review_refuse_handles_cannot_hide_in_lua_roots() {
    struct Refuse(MemoryFilesystem);
    impl Filesystem for Refuse {
        fn open(&self, p: &[u8], m: OpenMode) -> Completion<ResourceId> {
            self.0.open(p, m)
        }
        fn close(&self, id: ResourceId) -> Completion<()> {
            self.0.close(id)
        }
    }
    for source in [
        "local f=assert(io.open('f')); root=function() return f end",
        "local f=assert(io.open('f')); root=coroutine.create(function() return f end)",
        "debug.getregistry().hidden=assert(io.open('f'))",
        "io.input('f')",
        "collectgarbage('stop'); root=setmetatable({assert(io.open('f'))},{__mode='v'})",
    ] {
        let fs = Arc::new(Refuse(
            MemoryFilesystem::new([(b"f".to_vec(), vec![])], Default::default()).unwrap(),
        ));
        let mut r = runtime(
            source.as_bytes(),
            HostCapabilities::sandbox().filesystem(fs.clone()),
        );
        let mut j = Journal::new();
        run(&mut r, &mut j);
        assert!(
            matches!(r.snapshot(), Err(SnapshotError::NonPortableResource { .. })),
            "{source}"
        );
        r.begin_close().unwrap();
        run(&mut r, &mut j);
        assert_eq!(fs.0.open_count(), 0);
    }
}

#[test]
fn review_snapshot_rejects_aliased_resources_and_closed_read_work() {
    let fs = Arc::new(CountFs::new(b"hello".to_vec(), false));
    let caps = HostCapabilities::sandbox().filesystem(fs.clone());
    let h = host(caps.clone());
    let mut r = runtime(
        b"f=assert(io.open('f')); g=assert(io.open('f'))",
        caps.clone(),
    );
    let mut j = Journal::new();
    run(&mut r, &mut j);
    let mut image = r.to_image().unwrap();
    let mut resources = image
        .userdata
        .iter_mut()
        .filter_map(|u| match &mut u.payload {
            crate::snapshot::PayloadImage::File(f) if !f.closed && f.kind < 2 => Some(f),
            _ => None,
        });
    let first = resources.next().unwrap().id;
    resources.next().unwrap().id = first;
    let bytes = crate::snapshot::encode(&image).unwrap();
    assert!(Runtime::restore(&bytes, &h).is_err());
    r.begin_close().unwrap();
    run(&mut r, &mut j);
    assert_eq!(fs.fs.open_count(), 0);
    let mut r = runtime(b"local f <close> = assert(io.open('f')); f:read('a')", caps);
    let mut j = Journal::new();
    loop {
        assert!(matches!(r.run(1, &mut j).unwrap(), StepOutcome::Paused(_)));
        if r.heap.threads.iter().any(|(_,_,t)| t.frames.iter().any(|f| matches!(f.boundary(),Some(Boundary::Builtin { task:Task::Io(w),.. }) if matches!(w.as_ref(),IoWork::Read{..})))) { break; }
    }
    let mut image = r.to_image().unwrap();
    for u in &mut image.userdata {
        if let crate::snapshot::PayloadImage::File(f) = &mut u.payload {
            f.closed = true;
        }
    }
    assert!(Runtime::restore(&crate::snapshot::encode(&image).unwrap(), &h).is_err());
    run(&mut r, &mut j);
    assert_eq!(fs.fs.open_count(), 0);
}

#[test]
fn review_invalid_io_options_preserve_raw_bytes() {
    let mut r = runtime(br#"
        local f <close> = assert(io.open('f'))
        for _,method in ipairs{f.seek,f.setvbuf} do
            local ok,e=pcall(method,f,string.char(255)..'tail')
            assert(not ok and e:find("invalid option '"..string.char(255).."tail'",1,true))
            local ok,e=pcall(method,f,'bad'..string.char(0)..'ignored')
            assert(not ok and e:find("invalid option 'bad'",1,true) and not e:find('ignored',1,true))
        end
    "#, HostCapabilities::sandbox().filesystem(Arc::new(CountFs::new(vec![],false))));
    run(&mut r, &mut Journal::new());
}

#[test]
fn buffered_visibility_checkpoint_replay_matrix() {
    for mode in ["no", "full", "line"] {
        for gc in [GcMode::Incremental, GcMode::Generational] {
            for pending in [false, true] {
                for quantum in [1, 3, 7] {
                    let fs = Arc::new(CountFs::new(vec![], pending));
                    let caps = HostCapabilities::sandbox().filesystem(fs.clone());
                    let h = host(caps.clone());
                    let collector = if gc == GcMode::Incremental {
                        "incremental"
                    } else {
                        "generational"
                    };
                    let source = format!(
                        "collectgarbage('{collector}'); collectgarbage('stop'); f=assert(io.open('f','w+')); f:setvbuf('{mode}',16); f:write('one'); f:write('two\\nend'); f:flush()"
                    );
                    let mut r = runtime(source.as_bytes(), caps);
                    let mut j = Journal::new();
                    let mut images = Vec::new();
                    let mut saw_buffer = mode == "no";
                    let mut saw_wait = !pending;
                    let mut saw_committed = !pending;
                    loop {
                        let outcome = r.run(quantum, &mut j).unwrap();
                        let image = r.snapshot().unwrap();
                        let completion = if let StepOutcome::Waiting(key) = outcome {
                            Some((key, fs.answer.borrow().clone().unwrap()))
                        } else {
                            None
                        };
                        images.push((image.clone(), completion));
                        if let Some(f) =
                            r.heap
                                .userdata
                                .iter()
                                .find_map(|(_, _, u)| match &u.payload {
                                    Payload::File(f) if !f.closed && f.kind < 2 => Some(f),
                                    _ => None,
                                })
                        {
                            if mode != "no" && f.write_buffer == b"one" && f.write_flush == 0 {
                                saw_buffer = true;
                                assert_eq!(fs.fs.contents(b"f").unwrap(), b"");
                            }
                            let flushing = r.heap.threads.iter().any(|(_,_,t)| t.frames.iter().any(|frame| matches!(frame.boundary(), Some(Boundary::Builtin { task: Task::Io(work), .. }) if matches!(work.as_ref(), IoWork::Flush))));
                            if f.write_buffer == b"end" && !flushing {
                                assert_eq!(fs.fs.contents(b"f").unwrap(), b"onetwo\n");
                            }
                        }
                        r = Runtime::restore(&image, &h).unwrap();
                        match outcome {
                            StepOutcome::Waiting(key) => {
                                saw_wait |= fs.writes.get() > 0;
                                let fuel = r.fuel_consumed();
                                assert_eq!(r.run(100, &mut j).unwrap(), StepOutcome::Waiting(key));
                                assert_eq!(r.fuel_consumed(), fuel);
                                let answer = fs.answer.borrow_mut().take().unwrap();
                                r.complete_capability(key, answer).unwrap();
                                let image = r.snapshot().unwrap();
                                images.push((image.clone(), None));
                                r = Runtime::restore(&image, &h).unwrap();
                                saw_committed |= fs.writes.get() > 0;
                            }
                            StepOutcome::Paused(_) => {}
                            StepOutcome::Completed => break,
                            other => panic!("{mode}/{gc:?}/{pending}/{quantum}: {other:?}"),
                        }
                    }
                    // Quantum one visits the exact mid-buffer states; coarser
                    // schedules may pass through them within a single run.
                    if quantum == 1 {
                        assert!(saw_buffer);
                    }
                    assert!(saw_wait && saw_committed);
                    assert_eq!(fs.fs.contents(b"f").unwrap(), b"onetwo\nend");
                    let writes = fs.writes.get();
                    let fuel = r.fuel_consumed();
                    for (image, completion) in images {
                        let mut replay = Runtime::restore(&image, &h).unwrap();
                        loop {
                            match replay.run(1000, &mut j).unwrap() {
                                StepOutcome::Completed => break,
                                StepOutcome::Paused(_) => {}
                                StepOutcome::Waiting(key) => {
                                    let (saved, answer) = completion.clone().unwrap();
                                    assert_eq!(saved, key);
                                    replay.complete_capability(key, answer).unwrap();
                                }
                                other => panic!("{other:?}"),
                            }
                        }
                        assert_eq!(fs.writes.get(), writes, "effect repeated on replay");
                        assert_eq!(replay.fuel_consumed(), fuel);
                    }
                    r.begin_close().unwrap();
                    loop {
                        match r.run(1, &mut j).unwrap() {
                            StepOutcome::Waiting(key) => {
                                if let Some(id) = fs.pending_close.take() {
                                    assert!(matches!(fs.fs.close(id), Completion::Ready(Ok(()))));
                                }
                                r.complete_capability(key, fs.answer.borrow_mut().take().unwrap())
                                    .unwrap();
                                r = Runtime::restore(&r.snapshot().unwrap(), &h).unwrap();
                            }
                            StepOutcome::Paused(_) => {
                                r = Runtime::restore(&r.snapshot().unwrap(), &h).unwrap()
                            }
                            StepOutcome::Completed => break,
                            other => panic!("{other:?}"),
                        }
                    }
                    assert_eq!(fs.closes.get(), 1);
                }
            }
        }
    }
}

#[test]
fn buffered_shutdown_and_exit_flush_file_and_standard_output() {
    use crate::hostcaps::testing::MemoryStdio;
    for exit in [false, true] {
        let fs = Arc::new(CountFs::new(vec![], false));
        let stdio = Arc::new(MemoryStdio::new(vec![]));
        let caps = HostCapabilities::sandbox()
            .filesystem(fs.clone())
            .stdio(stdio.clone());
        let h = host(caps.clone());
        let source = if exit {
            b"f=assert(io.open('f','w')); f:write('file'); io.write('stdout'); os.exit(0,true)"
                .as_slice()
        } else {
            b"f=assert(io.open('f','w')); f:write('file'); io.write('stdout')".as_slice()
        };
        let mut r = runtime(source, caps);
        let mut j = Journal::new();
        if !exit {
            run(&mut r, &mut j);
            assert_eq!(fs.fs.contents(b"f").unwrap(), b"");
            assert_eq!(stdio.stdout(), b"");
            r.begin_close().unwrap();
        }
        loop {
            let outcome = r.run(1, &mut j).unwrap();
            r = Runtime::restore(&r.snapshot().unwrap(), &h).unwrap();
            match outcome {
                StepOutcome::Paused(_) => {}
                StepOutcome::Completed if !exit => break,
                StepOutcome::ExitRequested { close: true, .. } if exit => break,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(fs.fs.contents(b"f").unwrap(), b"file");
        assert_eq!(stdio.stdout(), b"stdout");
        assert_eq!(fs.writes.get(), 1);
        assert_eq!(fs.closes.get(), 1);
    }
}

#[test]
fn buffered_snapshot_charges_and_invariants() {
    let fs = Arc::new(CountFs::new(vec![], false));
    let caps = HostCapabilities::sandbox().filesystem(fs.clone());
    let h = host(caps.clone());
    let mut r = runtime(b"f=assert(io.open('f','w')); f:write('pending')", caps);
    let mut j = Journal::new();
    run(&mut r, &mut j);
    assert_eq!(fs.writes.get(), 0);
    let image = r.to_image().unwrap();
    let index = image.userdata.iter().position(|u| matches!(&u.payload, crate::snapshot::PayloadImage::File(f) if !f.closed && f.kind < 2)).unwrap();
    assert_eq!(image.userdata[index].charge, FILE_CHARGE + 7);
    for case in 0..7 {
        let mut image = image.clone();
        let u = &mut image.userdata[index];
        let crate::snapshot::PayloadImage::File(f) = &mut u.payload else {
            panic!()
        };
        match case {
            0 => u.charge -= 1,
            1 => f.closed = true,
            2 => f.cursor = 0,
            3 => f.write_flush = 8,
            4 => f.write_capacity = 0,
            5 => f.mode.write = false,
            _ => f.read_buffer = vec![b'x'],
        }
        assert!(Runtime::restore(&crate::snapshot::encode(&image).unwrap(), &h).is_err());
    }
    // New readers accept the old schema-25 file encoding with empty output.
    let crate::snapshot::PayloadImage::File(mut f) = image.userdata[index].payload.clone() else {
        panic!()
    };
    f.write_buffer.clear();
    let mut bytes = vec![];
    f.encode(&mut bytes);
    bytes[25] &= 127; // kind/id/mode/policy/cursor/closed, then buffering
    bytes.truncate(bytes.len() - 13);
    let decoded = FileState::decode(&mut bytes.as_slice()).unwrap();
    assert!(decoded.write_buffer.is_empty());
    r.begin_close().unwrap();
    run(&mut r, &mut j);
    assert_eq!(fs.fs.contents(b"f").unwrap(), b"pending");
}

#[test]
fn buffered_quota_failure_keeps_cursor_and_external_bytes_unchanged() {
    let fs = Arc::new(CountFs::new(vec![], false));
    let mut r = runtime(
        b"collectgarbage('stop'); f=assert(io.open('f','w')); s=string.rep('x',4095); f:write('',s)",
        HostCapabilities::sandbox().filesystem(fs.clone()),
    );
    let mut j = Journal::new();
    loop {
        assert!(matches!(r.run(1, &mut j).unwrap(), StepOutcome::Paused(_)));
        if r.heap.threads.iter().any(|(_,_,t)| t.frames.iter().any(|f| matches!(f.boundary(),Some(Boundary::Builtin { task:Task::Io(w),.. }) if matches!(w.as_ref(),IoWork::Write{next:2,offset:0,..})))) { break; }
    }
    let quota = r.heap.gc.quota;
    r.collect();
    r.heap.gc.quota = r.heap.gc.used + 4094;
    assert_eq!(
        r.run_until_terminal(100, &mut j).unwrap(),
        StepOutcome::LuaError(LuaFault::Memory)
    );
    assert_eq!(fs.writes.get(), 0);
    assert_eq!(fs.fs.contents(b"f").unwrap(), b"");
    let f = r
        .heap
        .userdata
        .iter()
        .find_map(|(_, _, u)| match &u.payload {
            Payload::File(f) if !f.closed => Some(f),
            _ => None,
        })
        .unwrap();
    assert_eq!(f.cursor, 0);
    assert!(f.write_buffer.is_empty());
    r.heap.gc.quota = quota;
    r.begin_close().unwrap();
    assert!(matches!(
        r.run_until_terminal(100, &mut j).unwrap(),
        StepOutcome::LuaError(_)
    ));
}

#[test]
fn buffered_flush_failure_still_closes_once_and_checks_late_arguments() {
    struct FailWrite {
        fs: MemoryFilesystem,
        writes: Cell<u32>,
        closes: Cell<u32>,
    }
    impl Filesystem for FailWrite {
        fn open(&self, p: &[u8], m: OpenMode) -> Completion<ResourceId> {
            self.fs.open(p, m)
        }
        fn write_at(&self, _: ResourceId, _: u64, _: &[u8]) -> Completion<usize> {
            self.writes.set(self.writes.get() + 1);
            Completion::Ready(Err(HostIoError::new(
                HostIoErrorKind::Other,
                b"write failed".to_vec(),
            )))
        }
        fn close(&self, id: ResourceId) -> Completion<()> {
            self.closes.set(self.closes.get() + 1);
            self.fs.close(id)
        }
        fn handle_policy(&self) -> HandlePolicy {
            HandlePolicy::Rebind
        }
        fn rebind(&self, id: ResourceId) -> Result<(), HostIoError> {
            self.fs.rebind(id)
        }
    }
    for action in [
        "local a,e,c=f:close(); assert(a==nil and e=='write failed' and c==5)",
        "assert(f:write('abc')==f); local a,e,c=f:flush(); assert(a==nil and e=='write failed'); assert(f:close())",
        "local ok,e=pcall(f.write,f,string.rep('x',8192),nil); assert(not ok and e:find('string expected',1,true)); assert(f:close())",
    ] {
        let fs = Arc::new(FailWrite {
            fs: MemoryFilesystem::new([(b"f".to_vec(), vec![])], Default::default()).unwrap(),
            writes: Cell::new(0),
            closes: Cell::new(0),
        });
        let caps = HostCapabilities::sandbox().filesystem(fs.clone());
        let h = host(caps.clone());
        let source = format!("f=assert(io.open('f','w')); f:write('pending'); {action}");
        let mut r = runtime(source.as_bytes(), caps);
        let mut j = Journal::new();
        loop {
            let outcome = r.run(1, &mut j).unwrap();
            r = Runtime::restore(&r.snapshot().unwrap(), &h).unwrap();
            match outcome {
                StepOutcome::Paused(_) => {}
                StepOutcome::Completed => break,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(fs.writes.get(), 1);
        assert_eq!(fs.closes.get(), 1);
        assert_eq!(fs.fs.contents(b"f").unwrap(), b"");
    }
}
