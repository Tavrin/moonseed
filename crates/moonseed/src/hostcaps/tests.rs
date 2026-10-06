use super::*;
use crate::{Config, Host, HostCapabilities, HostRegistry, Journal, Runtime, StepOutcome, VmError};
use std::sync::Arc;

fn ready<T>(c: Completion<T>) -> Result<T, HostIoError> {
    match c {
        Completion::Ready(r) => r,
        Completion::Pending(_) => panic!("unexpected wait"),
    }
}
fn runtime(caps: HostCapabilities) -> Runtime {
    let mut r = Runtime::boot_spin(Config::default(), HostRegistry::new()).unwrap();
    r.apply_capabilities(&caps);
    r
}
fn restored(bytes: &[u8], caps: HostCapabilities) -> Runtime {
    Runtime::restore(bytes, &Host::new(HostRegistry::new()).capabilities(caps)).unwrap()
}

#[test]
fn vfs_byte_paths_positional_lifecycle_and_limits() {
    let fs = MemoryFilesystem::new(
        [(b"x\0\xff".to_vec(), b"abc".to_vec())],
        MemoryOptions {
            max_open_files: 1,
            max_bytes_per_op: 4,
            max_file_bytes: 8,
            ..MemoryOptions::default()
        },
    )
    .unwrap();
    let mode = OpenMode::parse(b"r+b").unwrap();
    let id = ready(fs.open(b"x\0\xff", mode)).unwrap();
    assert_eq!(fs.handle_policy(), HandlePolicy::Rebind);
    fs.rebind(id).unwrap();
    assert_eq!(ready(fs.read_at(id, 1, 2)).unwrap(), b"bc");
    assert_eq!(ready(fs.read_at(id, 1, 2)).unwrap(), b"bc");
    assert_eq!(ready(fs.write_at(id, 5, b"z")).unwrap(), 1);
    assert_eq!(fs.contents(b"x\0\xff").unwrap(), b"abc\0\0z");
    assert_eq!(ready(fs.read_at(id, u64::MAX, 4)).unwrap(), b"");
    assert_eq!(
        ready(fs.open(b"new", OpenMode::parse(b"w").unwrap()))
            .unwrap_err()
            .kind,
        HostIoErrorKind::PermissionDenied
    );
    assert!(fs.contents(b"new").is_none());
    assert!(ready(fs.read_at(id, 0, 5)).is_err());
    assert!(ready(fs.write_at(id, 0, b"12345")).is_err());
    assert!(ready(fs.write_at(id, u64::MAX, b"x")).is_err());
    ready(fs.rename(b"x\0\xff", b"renamed")).unwrap();
    ready(fs.remove(b"renamed")).unwrap();
    assert_eq!(ready(fs.read_at(id, 0, 4)).unwrap(), b"abc\0");
    ready(fs.flush(id)).unwrap();
    ready(fs.close(id)).unwrap();
    assert!(fs.rebind(id).is_err());
    assert!(ready(fs.close(id)).is_err());
    let id = ready(fs.open(b"append", OpenMode::parse(b"a+").unwrap())).unwrap();
    assert_eq!(ready(fs.append(id, b"ab")).unwrap(), 2);
    assert_eq!(ready(fs.append(id, b"c")).unwrap(), 3);
    assert!(ready(fs.write_at(id, 0, b"q")).is_err());
    ready(fs.close(id)).unwrap();
    let id = ready(fs.temp_file()).unwrap();
    ready(fs.write_at(id, 0, b"temp")).unwrap();
    ready(fs.close(id)).unwrap();
    let a = ready(fs.temp_name()).unwrap();
    let b = ready(fs.temp_name()).unwrap();
    assert_ne!(a, b);
    assert_eq!(fs.contents(&a).unwrap(), b"");
    assert_eq!(fs.open_count(), 0);
    assert!(ready(fs.read_file(b"append", 2)).is_err());
    assert_eq!(ready(fs.read_file(b"append", 4)).unwrap(), b"abc");
    assert!(!ready(fs.probe_readable(b"missing")).unwrap());
    let ro = MemoryFilesystem::new(
        [(b"a".to_vec(), b"q".to_vec())],
        MemoryOptions {
            read_only: true,
            ..MemoryOptions::default()
        },
    )
    .unwrap();
    let id = ready(ro.open(b"a", OpenMode::parse(b"r").unwrap())).unwrap();
    assert!(ready(ro.write_at(id, 0, b"q")).is_err());
    assert!(ready(ro.remove(b"a")).is_err());
    assert!(ready(ro.rename(b"a", b"b")).is_err());
    assert!(ready(ro.temp_file()).is_err());
    assert!(ready(ro.temp_name()).is_err());
    assert!(ready(ro.open(b"a", OpenMode::parse(b"w").unwrap())).is_err());
    assert_eq!(ro.contents(b"a").unwrap(), b"q");
    ready(ro.close(id)).unwrap();
    for mode in [b"".as_slice(), b"rr", b"rb++", b"wx", b"r\0"] {
        assert!(OpenMode::parse(mode).is_err());
    }
}

#[test]
fn capability_journal_replays_reads_mutations_and_verifies_every_parameter() {
    let fs = Arc::new(
        MemoryFilesystem::new([(b"f".to_vec(), b"old".to_vec())], MemoryOptions::default())
            .unwrap(),
    );
    let caps = HostCapabilities::sandbox().filesystem(fs.clone());
    let mut r = runtime(caps.clone());
    let before = r.snapshot().unwrap();
    let mut j = Journal::new();
    let request = CapabilityRequest::FilesystemReadFile {
        path: b"f".to_vec(),
        max: 64,
    };
    assert_eq!(
        r.capability(&request, &mut j).unwrap(),
        CapabilityPoll::Ready(Ok(CapabilityValue::Bytes(b"old".to_vec())))
    );
    let id = ready(fs.open(b"f", OpenMode::parse(b"w+").unwrap())).unwrap();
    ready(fs.write_at(id, 0, b"new")).unwrap();
    ready(fs.close(id)).unwrap();
    let mut replay = restored(&before, caps.clone());
    let mut persisted = Journal::new();
    for rec in j.entries() {
        persisted.seed_record(rec.clone()).unwrap();
    }
    assert_eq!(
        replay.capability(&request, &mut persisted).unwrap(),
        CapabilityPoll::Ready(Ok(CapabilityValue::Bytes(b"old".to_vec())))
    );
    let mut mismatch = restored(&before, caps.clone());
    assert_eq!(
        mismatch.capability(
            &CapabilityRequest::FilesystemReadFile {
                path: b"g".to_vec(),
                max: 64
            },
            &mut persisted
        ),
        Err(VmError::Corrupt)
    );
    assert_eq!(persisted, j);
    let op = CapabilityRequest::FilesystemOpen {
        path: b"new-file".to_vec(),
        mode: OpenMode::parse(b"w+").unwrap(),
    };
    let before = r.snapshot().unwrap();
    let value = r.capability(&op, &mut j).unwrap();
    assert_eq!(fs.open_count(), 1);
    let mut replay = restored(&before, caps);
    assert_eq!(replay.capability(&op, &mut j).unwrap(), value);
    assert_eq!(fs.open_count(), 1);
    let e = j.entries().first().unwrap();
    assert_eq!(
        e.request.as_deref(),
        Some(request.request_bytes().as_slice())
    );
    assert!(
        j.replay_request::<Vec<u8>>(e.id, 1, &request.request_bytes())
            .is_err()
    );
}

#[test]
fn mocks_profiles_builder_restore_and_library_flags() {
    use testing::*;
    let env = Arc::new(MemoryEnvironment::new());
    env.set(b"n\0\xff".to_vec(), b"v\xff".to_vec());
    let stdio = Arc::new(MemoryStdio::new(b"abc".to_vec()));
    let process = Arc::new(MockProcess::new(ProcessStatus::Signal(9)));
    let caps = HostCapabilities::sandbox()
        .environment(env.clone())
        .stdio(stdio.clone())
        .clock(Arc::new(FixedClock { now: 123, cpu: 0.5 }))
        .civil(Arc::new(FixedCivilTime {
            offset: CivilOffset {
                seconds: 3600,
                isdst: false,
            },
        }))
        .process(process.clone());
    assert!(HostCapabilities::sandbox().filesystem.is_none());
    let built = Runtime::builder()
        .capabilities(caps.clone())
        .libraries(crate::Libraries::STANDARD)
        .build()
        .unwrap();
    let bytes = built.snapshot().unwrap();
    let mut reg = HostRegistry::new();
    crate::register_standard(&mut reg);
    let mut built = Runtime::restore(&bytes, &Host::new(reg).capabilities(caps.clone())).unwrap();
    let globals = built.globals();
    let io: crate::Table = globals.raw_get(&mut built, "io").unwrap();
    let os: crate::Table = globals.raw_get(&mut built, "os").unwrap();
    assert_eq!(io.raw_len(&built).unwrap(), 0);
    assert_eq!(os.raw_len(&built).unwrap(), 0);
    let mut r = runtime(caps);
    let mut j = Journal::new();
    assert_eq!(
        r.capability(
            &CapabilityRequest::EnvironmentGet {
                name: b"n\0\xff".to_vec()
            },
            &mut j
        )
        .unwrap(),
        CapabilityPoll::Ready(Ok(CapabilityValue::OptionalBytes(Some(b"v\xff".to_vec()))))
    );
    assert_eq!(
        r.capability(&CapabilityRequest::ClockCpuSeconds, &mut j)
            .unwrap(),
        CapabilityPoll::Ready(Ok(CapabilityValue::Number(0.5)))
    );
    assert_eq!(
        r.capability(
            &CapabilityRequest::CivilTimeUtcSeconds {
                local_seconds: 7200,
                isdst: None
            },
            &mut j
        )
        .unwrap(),
        CapabilityPoll::Ready(Ok(CapabilityValue::Integer(3600)))
    );
    r.capability(
        &CapabilityRequest::StdioWriteStdout {
            bytes: b"out".to_vec(),
        },
        &mut j,
    )
    .unwrap();
    assert_eq!(stdio.stdout(), b"out");
    r.capability(
        &CapabilityRequest::ProcessExecute {
            cmd: b"noop".to_vec(),
        },
        &mut j,
    )
    .unwrap();
    assert_eq!(process.commands(), vec![b"noop".to_vec()]);
    assert!(crate::Libraries::STANDARD.contains(crate::Libraries::IO | crate::Libraries::OS));
    assert!(crate::Libraries::ALL.contains(crate::Libraries::STANDARD | crate::Libraries::DEBUG));
}

#[test]
fn journal_request_api_keeps_legacy_records_and_rejects_mismatch_without_callback() {
    let id = crate::EffectId {
        domain: 9,
        sequence: 1,
    };
    let mut j = Journal::new();
    assert_eq!(j.commit_request(id, 2, b"request", || 42i64).unwrap(), 42);
    assert_eq!(
        j.commit_request(id, 2, b"request", || -> i64 { panic!("repeated") })
            .unwrap(),
        42i64
    );
    assert!(j.commit(id, 2, || 42i64).is_err());
    assert!(j.commit_request(id, 3, b"request", || 0i64).is_err());
    assert!(j.commit_request(id, 2, b"changed", || 0i64).is_err());
    assert!(j.commit_request(id, 2, b"request", Vec::new).is_err());
    let mut persisted = Journal::new();
    persisted.seed_record(j.entries()[0].clone()).unwrap();
    assert_eq!(persisted, j);
    let legacy = crate::EffectId {
        domain: 9,
        sequence: 2,
    };
    j.seed(legacy, 1, 7);
    assert_eq!(j.commit(legacy, 9, || 0i64).unwrap(), 7);
    assert!(j.commit_request(legacy, 1, b"x", || 0i64).is_err());
}

#[test]
fn every_capability_op_waits_checkpoints_restores_completes_and_commits_once() {
    let host = Arc::new(testing::PendingHost::new());
    let caps = HostCapabilities::sandbox()
        .filesystem(host.clone())
        .stdio(host.clone())
        .clock(host.clone())
        .civil(host.clone())
        .environment(host.clone())
        .process(host.clone());
    let operations = vec![
        (
            CapabilityRequest::FilesystemProbeReadable {
                path: b"x\0\xff".to_vec(),
            },
            CapabilityValue::Boolean(true),
        ),
        (
            CapabilityRequest::FilesystemOpen {
                path: b"x\0\xff".to_vec(),
                mode: OpenMode::parse(b"r+").unwrap(),
            },
            CapabilityValue::Resource(ResourceId(7)),
        ),
        (
            CapabilityRequest::FilesystemReadAt {
                id: ResourceId(7),
                offset: 2,
                max: 8,
            },
            CapabilityValue::Bytes(vec![]),
        ),
        (
            CapabilityRequest::FilesystemWriteAt {
                id: ResourceId(7),
                offset: 2,
                bytes: b"x\0\xff".to_vec(),
            },
            CapabilityValue::Unsigned(0),
        ),
        (
            CapabilityRequest::FilesystemAppend {
                id: ResourceId(7),
                bytes: b"x\0\xff".to_vec(),
            },
            CapabilityValue::Unsigned(0),
        ),
        (
            CapabilityRequest::FilesystemSize { id: ResourceId(7) },
            CapabilityValue::Unsigned(0),
        ),
        (
            CapabilityRequest::FilesystemFlush { id: ResourceId(7) },
            CapabilityValue::Unit,
        ),
        (
            CapabilityRequest::FilesystemClose { id: ResourceId(7) },
            CapabilityValue::Unit,
        ),
        (
            CapabilityRequest::FilesystemRemove {
                path: b"x\0\xff".to_vec(),
            },
            CapabilityValue::Unit,
        ),
        (
            CapabilityRequest::FilesystemRename {
                from: b"x\0\xff".to_vec(),
                to: b"x\0\xff".to_vec(),
            },
            CapabilityValue::Unit,
        ),
        (
            CapabilityRequest::FilesystemTempFile,
            CapabilityValue::Resource(ResourceId(7)),
        ),
        (
            CapabilityRequest::FilesystemTempName,
            CapabilityValue::Bytes(vec![]),
        ),
        (
            CapabilityRequest::FilesystemReadFile {
                path: b"x\0\xff".to_vec(),
                max: 8,
            },
            CapabilityValue::Bytes(vec![]),
        ),
        (
            CapabilityRequest::StdioReadStdin { max: 8 },
            CapabilityValue::Bytes(vec![]),
        ),
        (
            CapabilityRequest::StdioWriteStdout {
                bytes: b"x\0\xff".to_vec(),
            },
            CapabilityValue::Unsigned(0),
        ),
        (
            CapabilityRequest::StdioWriteStderr {
                bytes: b"x\0\xff".to_vec(),
            },
            CapabilityValue::Unsigned(0),
        ),
        (
            CapabilityRequest::StdioFlush {
                stream: Stream::Stdout,
            },
            CapabilityValue::Unit,
        ),
        (
            CapabilityRequest::ClockNowSeconds,
            CapabilityValue::Integer(12),
        ),
        (
            CapabilityRequest::ClockCpuSeconds,
            CapabilityValue::Number(0.25),
        ),
        (
            CapabilityRequest::CivilTimeLocalOffset { utc_seconds: 24 },
            CapabilityValue::Offset(CivilOffset {
                seconds: 0,
                isdst: false,
            }),
        ),
        (
            CapabilityRequest::CivilTimeUtcSeconds {
                local_seconds: 24,
                isdst: Some(false),
            },
            CapabilityValue::Integer(12),
        ),
        (
            CapabilityRequest::EnvironmentGet {
                name: b"x\0\xff".to_vec(),
            },
            CapabilityValue::OptionalBytes(None),
        ),
        (
            CapabilityRequest::ProcessShellAvailable,
            CapabilityValue::Boolean(true),
        ),
        (
            CapabilityRequest::ProcessExecute {
                cmd: b"x\0\xff".to_vec(),
            },
            CapabilityValue::Status(ProcessStatus::Exit(0)),
        ),
        (
            CapabilityRequest::ProcessPopen {
                cmd: b"x\0\xff".to_vec(),
                mode: PipeMode::Read,
            },
            CapabilityValue::Resource(ResourceId(7)),
        ),
        (
            CapabilityRequest::ProcessReadAt {
                id: ResourceId(7),
                offset: 2,
                max: 8,
            },
            CapabilityValue::Bytes(vec![]),
        ),
        (
            CapabilityRequest::ProcessWriteAt {
                id: ResourceId(7),
                offset: 2,
                bytes: b"x\0\xff".to_vec(),
            },
            CapabilityValue::Unsigned(0),
        ),
        (
            CapabilityRequest::ProcessFlush { id: ResourceId(7) },
            CapabilityValue::Unit,
        ),
        (
            CapabilityRequest::ProcessClose { id: ResourceId(7) },
            CapabilityValue::Status(ProcessStatus::Exit(0)),
        ),
    ];
    for (request, value) in operations {
        let mut r = runtime(caps.clone());
        let before = r.snapshot().unwrap();
        let mut j = Journal::new();
        let count = host.calls().len();
        let fuel = r.fuel_consumed();
        let CapabilityPoll::Waiting(key) = r.capability(&request, &mut j).unwrap() else {
            panic!("not waiting");
        };
        assert_eq!(host.calls().len(), count + 1);
        assert_eq!(r.run(100, &mut j).unwrap(), StepOutcome::Waiting(key));
        assert_eq!(r.fuel_consumed(), fuel);
        assert!(j.entries().is_empty());
        assert_eq!(
            r.capability(&request, &mut j).unwrap(),
            CapabilityPoll::Waiting(key)
        );
        assert_eq!(host.calls().len(), count + 1);
        let info = r.wait(key).unwrap();
        assert_eq!(info.operation, request.operation());
        assert_eq!(info.payload.len(), 3);
        let waiting = r.snapshot().unwrap();
        let mut r = restored(&waiting, caps.clone());
        assert_eq!(r.run(100, &mut j).unwrap(), StepOutcome::Waiting(key));
        assert!(r.complete(key, crate::Completion::Return(vec![])).is_err());
        r.complete_capability(key, Ok(value.clone())).unwrap();
        assert!(matches!(
            r.complete_capability(key, Ok(value.clone())),
            Err(crate::Error::Api(crate::ApiError::AlreadyCompleted))
        ));
        let completed = r.snapshot().unwrap();
        let mut r = restored(&completed, caps.clone());
        assert!(r.complete_capability(key, Ok(value.clone())).is_err());
        assert_eq!(
            r.capability(&request, &mut j).unwrap(),
            CapabilityPoll::Ready(Ok(value.clone()))
        );
        assert_eq!(j.entries().len(), 1);
        assert_eq!(host.calls().len(), count + 1);
        let mut replay = restored(&before, caps.clone());
        assert_eq!(
            replay.capability(&request, &mut j).unwrap(),
            CapabilityPoll::Ready(Ok(value))
        );
        assert_eq!(host.calls().len(), count + 1);
        assert_eq!(j.entries().len(), 1);
    }
}

#[test]
fn pending_error_bad_completion_and_request_mismatch_are_transactional() {
    let host = Arc::new(testing::PendingHost::new());
    let caps = HostCapabilities::sandbox().filesystem(host);
    let mut r = runtime(caps.clone());
    let mut j = Journal::new();
    let request = CapabilityRequest::FilesystemReadAt {
        id: ResourceId(1),
        offset: 0,
        max: 2,
    };
    let CapabilityPoll::Waiting(key) = r.capability(&request, &mut j).unwrap() else {
        panic!()
    };
    let before = r.snapshot().unwrap();
    assert!(
        r.complete_capability(key, Ok(CapabilityValue::Unit))
            .is_err()
    );
    assert!(
        r.complete_capability(key, Ok(CapabilityValue::Bytes(vec![0; 3])))
            .is_err()
    );
    assert_eq!(r.snapshot().unwrap(), before);
    assert_eq!(
        r.capability(
            &CapabilityRequest::FilesystemReadAt {
                id: ResourceId(1),
                offset: 1,
                max: 2
            },
            &mut j
        ),
        Err(VmError::Corrupt)
    );
    let error = HostIoError::new(HostIoErrorKind::NotFound, b"missing\0\xff".to_vec());
    r.complete_capability(key, Err(error.clone())).unwrap();
    assert_eq!(
        r.capability(&request, &mut j).unwrap(),
        CapabilityPoll::Ready(Err(error))
    );
}

#[test]
fn internal_builtin_resumes_in_executor_after_wait_restore_without_repeating_mutation() {
    let host = Arc::new(testing::PendingHost::new());
    let caps = HostCapabilities::sandbox().stdio(host.clone());
    let mut registry = HostRegistry::new();
    registry.register_builtin("test.capability", crate::host::Builtin::CapabilitySmoke);
    let chunk = crate::compile(b"local n=smoke(); return n").unwrap();
    let mut r = Runtime::load_chunk(Config::default(), registry.clone(), &chunk).unwrap();
    r.apply_capabilities(&caps);
    r.set_global_native("smoke", "test.capability").unwrap();
    let before = r.snapshot().unwrap();
    let mut journal = Journal::new();
    let StepOutcome::Waiting(key) = r.run(100, &mut journal).unwrap() else {
        panic!("not waiting")
    };
    assert_eq!(host.calls().len(), 1);
    let waiting = r.snapshot().unwrap();
    let restore_host = Host::new(registry).capabilities(caps);
    let mut r = Runtime::restore(&waiting, &restore_host).unwrap();
    r.complete_capability(key, Ok(CapabilityValue::Unsigned(5)))
        .unwrap();
    let completed = r.snapshot().unwrap();
    let mut r = Runtime::restore(&completed, &restore_host).unwrap();
    assert_eq!(r.run(100, &mut journal).unwrap(), StepOutcome::Completed);
    assert_eq!(journal.entries().len(), 1);
    assert_eq!(host.calls().len(), 1);
    assert_eq!(
        r.entry_results().unwrap(),
        vec![crate::value::Value::Integer(5)]
    );
    let pending_fuel = r.fuel_consumed();
    let mut ready = Runtime::restore(
        &before,
        &Host::new(restore_host.registry.clone()).stdio(Arc::new(testing::MemoryStdio::default())),
    )
    .unwrap();
    assert_eq!(
        ready.run(100, &mut Journal::new()).unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(
        pending_fuel,
        ready.fuel_consumed(),
        "waiting must not add execution fuel"
    );
    let mut replay = Runtime::restore(&before, &restore_host).unwrap();
    assert_eq!(
        replay.run(100, &mut journal).unwrap(),
        StepOutcome::Completed
    );
    assert_eq!(journal.entries().len(), 1);
    assert_eq!(host.calls().len(), 1);
}

#[test]
fn malformed_capability_snapshots_are_refused_before_restore() {
    let caps = HostCapabilities::sandbox().filesystem(Arc::new(testing::PendingHost::new()));
    let mut r = runtime(caps.clone());
    let mut j = Journal::new();
    let req = CapabilityRequest::FilesystemReadFile {
        path: b"file".to_vec(),
        max: 10,
    };
    let CapabilityPoll::Waiting(key) = r.capability(&req, &mut j).unwrap() else {
        panic!()
    };
    let waiting = r.snapshot().unwrap();
    for mutation in 0..6 {
        let mut image =
            crate::snapshot::decode_within(&waiting, &crate::Limits::default()).unwrap();
        let frame = image
            .threads
            .iter_mut()
            .find_map(|t| {
                t.frames.last_mut().filter(|f| {
                    matches!(f.pending, crate::snapshot::PendingImage::Capability { .. })
                })
            })
            .unwrap();
        match mutation {
            0 => {
                if let crate::snapshot::PendingImage::Capability { sequence, .. } =
                    &mut frame.pending
                {
                    *sequence = image.next_sequence;
                }
            }
            1 => {
                if let crate::snapshot::PendingImage::Capability { wait_key, .. } =
                    &mut frame.pending
                {
                    *wait_key += 1;
                }
            }
            2 => {
                if let crate::snapshot::PendingImage::Capability { completed, .. } =
                    &mut frame.pending
                {
                    *completed = true;
                }
            }
            3 => frame.wait_request.as_mut().unwrap().0 = "capability.filesystem.remove".into(),
            4 => frame.wait_request.as_mut().unwrap().1[0] = crate::snapshot::EncValue::Nil,
            5 => frame.wait_request.as_mut().unwrap().1[2] = crate::snapshot::EncValue::Bool(true),
            _ => unreachable!(),
        }
        let bytes = crate::snapshot::encode(&image).unwrap();
        assert!(
            Runtime::restore(
                &bytes,
                &Host::new(HostRegistry::new()).capabilities(caps.clone())
            )
            .is_err(),
            "mutation {mutation}"
        );
    }
    r.complete_capability(key, Ok(CapabilityValue::Bytes(b"x".to_vec())))
        .unwrap();
    let completed = r.snapshot().unwrap();
    let mut image = crate::snapshot::decode_within(&completed, &crate::Limits::default()).unwrap();
    image.completed_waits.clear();
    image.last_completed_wait = None;
    assert!(
        Runtime::restore(
            &crate::snapshot::encode(&image).unwrap(),
            &Host::new(HostRegistry::new()).capabilities(caps)
        )
        .is_err()
    );
}

#[test]
fn capability_limits_prevent_invocation_and_pending_requests_survive_collection() {
    let host = Arc::new(testing::PendingHost::new());
    let caps = HostCapabilities::sandbox().stdio(host.clone());
    let mut r = runtime(caps.clone());
    let mut j = Journal::new();
    assert!(
        r.capability(
            &CapabilityRequest::StdioWriteStdout {
                bytes: vec![0; 64 * 1024 + 1]
            },
            &mut j
        )
        .is_err()
    );
    assert!(host.calls().is_empty());
    assert!(j.entries().is_empty());
    let req = CapabilityRequest::StdioWriteStdout {
        bytes: b"owned".to_vec(),
    };
    let CapabilityPoll::Waiting(key) = r.capability(&req, &mut j).unwrap() else {
        panic!()
    };
    r.collect();
    let waiting = r.snapshot().unwrap();
    let mut r = restored(&waiting, caps);
    r.complete_capability(key, Ok(CapabilityValue::Unsigned(5)))
        .unwrap();
    r.collect();
    assert_eq!(
        r.capability(&req, &mut j).unwrap(),
        CapabilityPoll::Ready(Ok(CapabilityValue::Unsigned(5)))
    );
    assert_eq!(host.calls().len(), 1);
}

#[test]
fn capability_waits_resume_tail_metamethod_close_finalizer_and_hook_calls() {
    for source in [
        "return smoke()",
        "local t=setmetatable({}, {__len=smoke}); return #t",
        "do local f <close> = setmetatable({}, {__close=smoke}) end return 5",
        "pcall(function() local f <close> = setmetatable({}, {__close=smoke}); error('x') end); return 5",
        "local f=setmetatable({}, {__gc=smoke}); f=nil; collectgarbage(); return 5",
        "debug.sethook(function() end, 'cr'); local n=smoke(); debug.sethook(); return n",
    ] {
        let host = Arc::new(testing::PendingHost::new());
        let caps = HostCapabilities::sandbox().stdio(host.clone());
        let mut registry = HostRegistry::new();
        crate::register_base(&mut registry);
        crate::register_debug(&mut registry);
        registry.register_builtin("test.capability", crate::host::Builtin::CapabilitySmoke);
        let chunk = crate::compile(source.as_bytes()).unwrap();
        let mut r = Runtime::load_chunk(
            Config {
                auto_gc: false,
                ..Config::default()
            },
            registry.clone(),
            &chunk,
        )
        .unwrap();
        r.apply_capabilities(&caps);
        r.install_base().unwrap();
        r.install_debug().unwrap();
        r.set_global_native("smoke", "test.capability").unwrap();
        let mut j = Journal::new();
        let StepOutcome::Waiting(key) = r.run(10000, &mut j).unwrap() else {
            panic!("not waiting: {source}")
        };
        assert_eq!(host.calls().len(), 1);
        let restore_host = Host::new(registry).capabilities(caps);
        let mut r = Runtime::restore(&r.snapshot().unwrap(), &restore_host)
            .unwrap_or_else(|e| panic!("{source}: {e:?}"));
        r.complete_capability(key, Ok(CapabilityValue::Unsigned(5)))
            .unwrap();
        let mut r = Runtime::restore(&r.snapshot().unwrap(), &restore_host)
            .unwrap_or_else(|e| panic!("{source}: {e:?}"));
        assert_eq!(
            r.run(10000, &mut j).unwrap(),
            StepOutcome::Completed,
            "{source}"
        );
        assert_eq!(
            r.entry_results().unwrap(),
            vec![crate::value::Value::Integer(5)],
            "{source}"
        );
        assert_eq!(host.calls().len(), 1);
        assert_eq!(j.entries().len(), 1);
    }
}
