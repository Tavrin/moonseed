//! Integrated checkpoint/replay matrix, beyond the Lua byte oracle's reach.
#![allow(clippy::arc_with_non_send_sync)] // Public capabilities do not require Send/Sync.
use crate::hostcaps::Completion;
use crate::hostcaps::testing::MockProcess;
use crate::hostproof::{self, capabilities, registry, runtime};
use crate::{
    CapabilityValue, Filesystem, GcMode, HandlePolicy, Host, HostIoError, Journal,
    MemoryFilesystem, OpenMode, PendingToken, ProcessStatus, ResourceId, Runtime, StepOutcome,
};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::Arc;

struct CountFs {
    fs: MemoryFilesystem,
    calls: RefCell<BTreeMap<&'static str, usize>>,
    pending: bool,
    close: Cell<Option<ResourceId>>,
    answer: RefCell<Option<Result<CapabilityValue, HostIoError>>>,
}
impl CountFs {
    fn new(pending: bool) -> Self {
        Self {
            fs: hostproof::filesystem(),
            calls: RefCell::new(BTreeMap::new()),
            pending,
            close: Cell::new(None),
            answer: RefCell::new(None),
        }
    }
    fn count(&self, op: &'static str) {
        *self.calls.borrow_mut().entry(op).or_default() += 1;
    }
    fn done<T>(
        &self,
        op: &'static str,
        result: Completion<T>,
        encode: impl FnOnce(T) -> CapabilityValue,
    ) -> Completion<T> {
        self.count(op);
        if !self.pending {
            return result;
        }
        let Completion::Ready(result) = result else {
            panic!("VFS pending")
        };
        assert!(self.answer.borrow().is_none());
        *self.answer.borrow_mut() = Some(result.map(encode));
        Completion::Pending(PendingToken(1))
    }
    fn finish(&self) -> Result<CapabilityValue, HostIoError> {
        if let Some(id) = self.close.take() {
            assert!(matches!(self.fs.close(id), Completion::Ready(Ok(()))));
        }
        self.answer.borrow_mut().take().unwrap()
    }
}
impl Filesystem for CountFs {
    fn probe_readable(&self, p: &[u8]) -> Completion<bool> {
        self.done("probe", self.fs.probe_readable(p), CapabilityValue::Boolean)
    }
    fn open(&self, p: &[u8], m: OpenMode) -> Completion<ResourceId> {
        self.done("open", self.fs.open(p, m), CapabilityValue::Resource)
    }
    fn read_at(&self, id: ResourceId, o: u64, n: usize) -> Completion<Vec<u8>> {
        self.done("read", self.fs.read_at(id, o, n), CapabilityValue::Bytes)
    }
    fn write_at(&self, id: ResourceId, o: u64, b: &[u8]) -> Completion<usize> {
        self.done("write", self.fs.write_at(id, o, b), |n| {
            CapabilityValue::Unsigned(n as u64)
        })
    }
    fn size(&self, id: ResourceId) -> Completion<u64> {
        self.done("size", self.fs.size(id), CapabilityValue::Unsigned)
    }
    fn flush(&self, id: ResourceId) -> Completion<()> {
        self.done("flush", self.fs.flush(id), |_| CapabilityValue::Unit)
    }
    fn close(&self, id: ResourceId) -> Completion<()> {
        if self.pending {
            self.close.set(Some(id));
            self.done("close", Completion::Ready(Ok(())), |_| {
                CapabilityValue::Unit
            })
        } else {
            self.done("close", self.fs.close(id), |_| CapabilityValue::Unit)
        }
    }
    fn read_file_range(&self, p: &[u8], o: u64, n: usize) -> Completion<Vec<u8>> {
        self.done(
            "source",
            self.fs.read_file_range(p, o, n),
            CapabilityValue::Bytes,
        )
    }
    fn handle_policy(&self) -> HandlePolicy {
        HandlePolicy::Rebind
    }
    fn rebind(&self, id: ResourceId) -> Result<(), HostIoError> {
        self.fs.rebind(id)
    }
}
fn restore(r: &Runtime, h: &Host) -> Runtime {
    let bytes = r.snapshot().unwrap();
    let restored = Runtime::restore(&bytes, h).unwrap();
    assert_eq!(bytes, restored.snapshot().unwrap());
    restored
}
fn drive(
    r: &mut Runtime,
    j: &mut Journal,
    h: &Host,
    fs: &CountFs,
    q: u64,
    checkpoint: bool,
) -> usize {
    let mut pauses = 0;
    for _ in 0..20000 {
        let outcome = r.run(q, j).unwrap();
        if checkpoint {
            *r = restore(r, h);
        }
        match outcome {
            StepOutcome::Completed => return pauses,
            StepOutcome::Paused(_) => pauses += 1,
            StepOutcome::Waiting(key) => {
                let fuel = r.fuel_consumed();
                assert_eq!(r.run(100, j).unwrap(), StepOutcome::Waiting(key));
                assert_eq!(fuel, r.fuel_consumed());
                r.complete_capability(key, fs.finish()).unwrap();
                if checkpoint {
                    *r = restore(r, h);
                }
                pauses += 1;
            }
            other => panic!("{other:?}: {:?}", r.lua_error()),
        }
    }
    panic!("matrix did not finish")
}
#[test]
fn host_checkpoint_matrix_quanta_both_collectors_ready_and_pending() {
    for mode in [GcMode::Incremental, GcMode::Generational] {
        let mut ready_entries = None;
        for pending in [false, true] {
            let fs = Arc::new(CountFs::new(pending));
            let p = Arc::new(MockProcess::new(ProcessStatus::Exit(3)));
            let caps = capabilities(fs.clone(), p.clone());
            let h = Host::new(registry()).capabilities(caps.clone());
            let mut straight = runtime(hostproof::SOURCE, caps, mode);
            let mut expected = Journal::new();
            drive(&mut straight, &mut expected, &h, &fs, u64::MAX, false);
            assert_eq!(fs.fs.open_count(), 1);
            let run_fuel = straight.fuel_consumed();
            straight.begin_close().unwrap();
            drive(&mut straight, &mut expected, &h, &fs, u64::MAX, false);
            let calls = fs.calls.borrow().clone();
            assert_eq!(calls["write"], 6);
            assert_eq!(calls["close"], 6);
            assert_eq!(fs.fs.open_count(), 0);
            if let Some(entries) = &ready_entries {
                assert_eq!(expected.entries(), entries);
            } else {
                ready_entries = Some(expected.entries().to_vec());
            }
            // Pending completion stores extra Lua strings. Their collector timing
            // can move closed-handle finalizers across begin_close; compare fuel
            // against the matching uninterrupted host, and effects across both.
            for q in [1, 2, 3, 7] {
                let fs = Arc::new(CountFs::new(pending));
                let p = Arc::new(MockProcess::new(ProcessStatus::Exit(3)));
                let caps = capabilities(fs.clone(), p.clone());
                let h = Host::new(registry()).capabilities(caps.clone());
                let mut r = runtime(hostproof::SOURCE, caps, mode);
                let mut j = Journal::new();
                assert!(
                    drive(&mut r, &mut j, &h, &fs, q, true) > 20,
                    "{mode:?} q={q} pending={pending}"
                );
                assert_eq!(fs.fs.open_count(), 1);
                assert_eq!(
                    r.fuel_consumed(),
                    run_fuel,
                    "run {mode:?} q={q} pending={pending}"
                );
                r.begin_close().unwrap();
                drive(&mut r, &mut j, &h, &fs, q, true);
                assert_eq!(fs.fs.open_count(), 0);
                assert_eq!(
                    *fs.calls.borrow(),
                    calls,
                    "{mode:?} q={q} pending={pending}"
                );
                assert_eq!(p.commands(), [b"mock command".to_vec()]);
                assert_eq!(fs.fs.contents(b"data"), Some(vec![b'x'; 150000]));
                assert_eq!(
                    r.fuel_consumed(),
                    straight.fuel_consumed(),
                    "{mode:?} q={q} pending={pending}"
                );
                assert_eq!(j.entries(), expected.entries());
            }
        }
    }
}

#[test]
fn host_replay_adversary_before_and_after_read_and_write_commits() {
    for mode in [GcMode::Incremental, GcMode::Generational] {
        for q in [1, 2, 3, 7] {
            let fs = Arc::new(CountFs::new(false));
            let p = Arc::new(MockProcess::new(ProcessStatus::Exit(3)));
            let caps = capabilities(fs.clone(), p);
            let h = Host::new(registry()).capabilities(caps.clone());
            let mut r=runtime(br#"f=assert(io.open('data','r+')); old=f:read('a'); f:seek('set'); f:write('written'); f:seek('set')"#,caps,mode);
            let mut j = Journal::new();
            let mut images = Vec::new();
            let mut previous = BTreeMap::new();
            loop {
                // Inspect every semantic work unit even when replay uses larger quanta.
                let outcome = r.run(1, &mut j).unwrap();
                let calls = fs.calls.borrow().clone();
                if calls != previous {
                    images.push(r.snapshot().unwrap());
                    previous = calls;
                }
                if outcome == StepOutcome::Completed {
                    break;
                }
                assert!(matches!(outcome, StepOutcome::Paused(_)));
            }
            assert!(images.len() >= 3);
            let counts = fs.calls.borrow().clone();
            let Completion::Ready(Ok(id)) = fs.fs.open(b"data", OpenMode::parse(b"w").unwrap())
            else {
                panic!()
            };
            assert!(matches!(
                fs.fs.write_at(id, 0, b"changed"),
                Completion::Ready(Ok(7))
            ));
            assert!(matches!(fs.fs.close(id), Completion::Ready(Ok(()))));
            for image in images {
                let mut replay = Runtime::restore(&image, &h).unwrap();
                let mut history = Journal::new();
                for entry in j.entries() {
                    history.seed_record(entry.clone()).unwrap();
                }
                drive(&mut replay, &mut history, &h, &fs, q, true);
                assert_eq!(*fs.calls.borrow(), counts);
                let old: crate::LuaString = replay.globals().raw_get(&mut replay, "old").unwrap();
                assert_eq!(old.as_bytes(&replay).unwrap(), b"one\ntwo\n");
                assert_eq!(fs.fs.contents(b"data"), Some(b"changed".to_vec()));
                replay
                    .load_main(
                        &crate::compile(
                            b"assert(f:seek('set')==0); assert(f:read('a')=='changed')",
                        )
                        .unwrap(),
                    )
                    .unwrap();
                drive(&mut replay, &mut history, &h, &fs, q, true);
                // New effects see changed bytes; undo only this test's counters.
                *fs.calls.borrow_mut() = counts.clone();
            }
            r.load_main(&crate::compile(b"f:close()").unwrap()).unwrap();
            drive(&mut r, &mut j, &h, &fs, q, true);
            assert_eq!(fs.fs.open_count(), 0);
        }
    }
}
#[test]
fn host_capability_native_fingerprint_is_repeatable() {
    let first = crate::host_capabilities_fingerprint().unwrap();
    assert_ne!(first, i64::MIN);
    assert_eq!(first, crate::host_capabilities_fingerprint().unwrap());
}

#[test]
fn buffered_checkpoint_replay_and_new_reads_after_world_change() {
    for mode in [GcMode::Incremental, GcMode::Generational] {
        for pending in [false, true] {
            let fs = Arc::new(CountFs::new(pending));
            let p = Arc::new(MockProcess::new(ProcessStatus::Exit(0)));
            let caps = capabilities(fs.clone(), p);
            let h = Host::new(registry()).capabilities(caps.clone());
            let mut r = runtime(
                b"f=assert(io.open('data')); assert(f:read('l')=='one')",
                caps,
                mode,
            );
            let mut j = Journal::new();
            drive(&mut r, &mut j, &h, &fs, 1, true);
            let image = r.snapshot().unwrap();
            let counts = fs.calls.borrow().clone();
            assert_eq!(counts["read"], 1);
            let before = r.memory().logical_bytes;
            let Completion::Ready(Ok(id)) = fs.fs.open(b"data", OpenMode::parse(b"w").unwrap())
            else {
                panic!()
            };
            assert!(matches!(
                fs.fs.write_at(id, 0, b"new\nfresh\n"),
                Completion::Ready(Ok(10))
            ));
            assert!(matches!(fs.fs.close(id), Completion::Ready(Ok(()))));
            r.load_main(&crate::compile(b"assert(f:read('l')=='two'); assert(f:seek('cur')==8); assert(f:read('l')=='h')").unwrap()).unwrap();
            drive(&mut r, &mut j, &h, &fs, 1, true);
            assert_eq!(fs.calls.borrow()["read"], 2);
            let mut replay = Runtime::restore(&image, &h).unwrap();
            replay.load_main(&crate::compile(b"assert(f:read('l')=='two'); assert(f:seek('cur')==8); assert(f:read('l')=='h')").unwrap()).unwrap();
            drive(&mut replay, &mut j, &h, &fs, 3, true);
            assert_eq!(
                fs.calls.borrow()["read"],
                2,
                "buffer and committed refill replay"
            );
            replay
                .load_main(
                    &crate::compile(
                        b"assert(f:seek('set')==0); assert(f:read('l')=='new'); f:close()",
                    )
                    .unwrap(),
                )
                .unwrap();
            drive(&mut replay, &mut j, &h, &fs, 2, true);
            assert_eq!(fs.calls.borrow()["read"], 3);
            assert_eq!(fs.fs.open_count(), 0);
            assert!(before > 8, "read-ahead is charged");
        }
    }
}

#[test]
fn buffered_reads_cross_refills_counts_numerals_and_update_handles() {
    let fs = Arc::new(CountFs::new(false));
    // Tiny lines cross a 16 KiB boundary; long lines exercise bounded work.
    let mut data = b"12 rest\n".repeat(3000);
    data.extend(vec![b'x'; 40000]);
    data.push(b'\n');
    let Completion::Ready(Ok(id)) = fs.fs.open(b"data", OpenMode::parse(b"w").unwrap()) else {
        panic!()
    };
    assert!(matches!(
        fs.fs.write_at(id, 0, &data),
        Completion::Ready(Ok(_))
    ));
    assert!(matches!(fs.fs.close(id), Completion::Ready(Ok(()))));
    let caps = capabilities(
        fs.clone(),
        Arc::new(MockProcess::new(ProcessStatus::Exit(0))),
    );
    let h = Host::new(registry()).capabilities(caps.clone());
    let mut r = runtime(
        br#"
        local f=assert(io.open('data','r+'))
        for i=1,3000 do assert(f:read('n')==12); assert(f:read('l')==' rest') end
        assert(#f:read('L')==40001); assert(f:read(0)==nil)
        assert(f:seek('set')==0); assert(f:read(0)==''); assert(f:read(2)=='12')
        assert(f:write('AB')==f); assert(f:read(4)=='est\n')
        assert(f:seek('set')==0); assert(f:read('l')=='12ABest')
        local g=assert(io.open('data','r+')); assert(g:read('l')=='12ABest')
        f:seek('set',8); f:write('changed\n')
        assert(g:read('l')=='12 rest') -- each handle owns its read-ahead, like stdio
        assert(g:seek('set',8)==8); assert(g:read('l')=='12 rest') -- pending output is still private
        assert(f:flush()); assert(g:seek('set',8)==8); assert(g:read('l')=='changed')
        f:seek('set'); assert(f:read('n')==12)
        g:seek('set',2); g:write('xy'); g:flush()
        assert(f:flush()); assert(f:read('l')=='xyest')
        g:close(); f:close()
    "#,
        caps,
        GcMode::Generational,
    );
    drive(&mut r, &mut Journal::new(), &h, &fs, u64::MAX, false);
    // Four data refills, one EOF probe, eight explicit seek/write/flush refills.
    assert_eq!(fs.calls.borrow()["read"], 13);
}

#[test]
fn buffered_stdin_checkpoint_retains_consumed_host_bytes() {
    use crate::hostcaps::testing::MemoryStdio;
    let streams = Arc::new(MemoryStdio::new(b"one\ntwo\n".to_vec()));
    let caps = crate::HostCapabilities::sandbox().stdio(streams);
    let h = Host::new(registry()).capabilities(caps.clone());
    let mut r = runtime(b"assert(io.read('l')=='one')", caps, GcMode::Generational);
    let mut j = Journal::new();
    assert_eq!(
        r.run_until_terminal(1, &mut j).unwrap(),
        StepOutcome::Completed
    );
    let mut r = Runtime::restore(&r.snapshot().unwrap(), &h).unwrap();
    r.load_main(
        &crate::compile(b"assert(io.read('l')=='two'); assert(io.read('l')==nil)").unwrap(),
    )
    .unwrap();
    assert_eq!(
        r.run_until_terminal(1, &mut j).unwrap(),
        StepOutcome::Completed
    );
}

#[test]
fn buffered_snapshot_rejects_invalid_position_and_unpaid_bytes() {
    let fs = Arc::new(CountFs::new(false));
    let caps = capabilities(
        fs.clone(),
        Arc::new(MockProcess::new(ProcessStatus::Exit(0))),
    );
    let h = Host::new(registry()).capabilities(caps.clone());
    let mut r = runtime(
        b"f=assert(io.open('data')); assert(f:read('l')=='one')",
        caps,
        GcMode::Incremental,
    );
    let mut j = Journal::new();
    drive(&mut r, &mut j, &h, &fs, 1, true);
    let original = r.to_image().unwrap();
    let index = original
        .userdata
        .iter()
        .position(|u| {
            matches!(&u.payload,
        crate::snapshot::PayloadImage::File(f) if !f.closed && f.kind == 0)
        })
        .unwrap();
    let crate::snapshot::PayloadImage::File(file) = &original.userdata[index].payload else {
        panic!()
    };
    assert_eq!(file.read_pos, 4);
    assert_eq!(file.read_buffer, b"one\ntwo\n");
    assert_eq!(
        original.userdata[index].charge,
        crate::iolib::FILE_CHARGE + 8
    );
    let mut image = original.clone();
    image.userdata[index].charge -= 1;
    assert!(matches!(
        Runtime::restore(&crate::snapshot::encode(&image).unwrap(), &h),
        Err(crate::Error::Vm(crate::VmError::Snapshot(
            crate::SnapshotError::UserdataCharge
        )))
    ));
    for case in 0..3 {
        let mut image = original.clone();
        let crate::snapshot::PayloadImage::File(file) = &mut image.userdata[index].payload else {
            panic!()
        };
        match case {
            0 => file.read_pos = 9,
            1 => file.closed = true,
            _ => file.cursor = 0,
        }
        assert!(Runtime::restore(&crate::snapshot::encode(&image).unwrap(), &h).is_err());
    }
    r.load_main(&crate::compile(b"f:close()").unwrap()).unwrap();
    drive(&mut r, &mut j, &h, &fs, 1, true);
    let closed = r.to_image().unwrap();
    let crate::snapshot::PayloadImage::File(file) = &closed.userdata[index].payload else {
        panic!()
    };
    assert!(file.read_buffer.is_empty());
    assert_eq!(closed.userdata[index].charge, crate::iolib::FILE_CHARGE);
}
