//! Portable host-capability proof shared with the scalar Wasm probe.
#![allow(clippy::arc_with_non_send_sync)] // Capability traits do not require Send/Sync.
use crate::hostcaps::testing::{FixedClock, MemoryEnvironment, MockProcess};
use crate::{
    Config, GcMode, Host, HostCapabilities, HostRegistry, Journal, Libraries, MemoryFilesystem,
    ProcessStatus, Runtime, StepOutcome, VmError,
};
use std::sync::Arc;

pub(crate) const SOURCE: &[u8] = br#"
    local b=assert(io.open('buffer','w+'))
    b:setvbuf('full',16); b:write('pending')
    local g=assert(io.open('buffer','r')); assert(g:read('a')==''); g:close()
    b:setvbuf('line',16); b:write('\ntail')
    local g=assert(io.open('buffer','r')); assert(g:read('a')=='pending\n'); g:close()
    assert(b:flush()); b:close()
    local f=assert(io.open('data','r+'))
    assert(f:read('l')=='one'); assert(f:seek('set')==0)
    local it=f:lines(); assert(it()=='one'); assert(it()=='two')
    assert(f:seek('set')==0)
    local s=string.rep('x',150000)
    assert(f:write(s)==f); assert(f:flush()); assert(f:seek('set')==0)
    assert(f:read('a')==s); assert(f:seek('end',-3)==149997)
    assert(f:read(3)=='xxx'); assert(f:close())
    local n=0
    for line in io.lines('lines') do n=n+#line; if n==6 then break end end
    assert(n==6)
    assert(assert(loadfile('large'))()==37)
    assert(dofile('dofile')==19)
    assert(package.searchpath('module','missing/?.lua;?.lua')=='module.lua')
    local m,p=require('module'); assert(m==23 and p=='module.lua')
    assert(require('module')==23)
    assert(os.time()==946684800 and os.clock()==0.5)
    assert(os.getenv('FIXED')=='value')
    local ok,kind,code=os.execute('mock command')
    assert(ok==nil and kind=='exit' and code==3)
    held=assert(io.open('lines'))
    fingerprint=n+37+19+m+os.time()
"#;

pub(crate) fn filesystem() -> MemoryFilesystem {
    let mut large = vec![b' '; 130000];
    large.extend_from_slice(b"return 37");
    MemoryFilesystem::new(
        [
            (b"data".to_vec(), b"one\ntwo\n".to_vec()),
            (b"lines".to_vec(), b"abc\ndef\nghi\n".to_vec()),
            (b"large".to_vec(), large),
            (
                b"dofile".to_vec(),
                b"local n=0 for i=1,5 do n=n+i end return n+4".to_vec(),
            ),
            (b"module.lua".to_vec(), b"return 23".to_vec()),
        ],
        Default::default(),
    )
    .unwrap()
}
pub(crate) fn capabilities(
    fs: Arc<dyn crate::Filesystem>,
    process: Arc<MockProcess>,
) -> HostCapabilities {
    let env = MemoryEnvironment::new();
    env.set(b"FIXED".to_vec(), b"value".to_vec());
    HostCapabilities::sandbox()
        .filesystem(fs)
        .clock(Arc::new(FixedClock {
            now: 946684800,
            cpu: 0.5,
        }))
        .environment(Arc::new(env))
        .process(process)
}
pub(crate) fn registry() -> HostRegistry {
    let mut reg = HostRegistry::new();
    crate::register_standard(&mut reg);
    crate::register_debug(&mut reg);
    reg
}
pub(crate) fn runtime(source: &[u8], caps: HostCapabilities, mode: GcMode) -> Runtime {
    let mut r = Runtime::builder()
        .config(Config {
            gc_mode: mode,
            ..Default::default()
        })
        .registry(registry())
        .libraries(Libraries::ALL)
        .capabilities(caps)
        .package_paths(b"missing/?.lua;?.lua", b"")
        .build()
        .unwrap();
    r.load_main(&crate::compile(source).unwrap()).unwrap();
    r
}

/// VFS IO, loaders, package, mock OS and mid-operation checkpoints.
/// The fingerprint folds actual file bytes, journal requests/outcomes, and fuel.
pub(crate) fn fingerprint() -> Result<i64, VmError> {
    let fs = Arc::new(filesystem());
    let process = Arc::new(MockProcess::new(ProcessStatus::Exit(3)));
    let caps = capabilities(fs.clone(), process.clone());
    let host = Host::new(registry()).capabilities(caps.clone());
    let mut r = runtime(SOURCE, caps, GcMode::Incremental);
    let mut j = Journal::new();
    let mut hash = 0i64;
    let mut fold = |bytes: &[u8]| {
        for &b in bytes {
            hash = hash.wrapping_mul(131).wrapping_add(i64::from(b));
        }
    };
    loop {
        let outcome = r.run(7, &mut j)?;
        let bytes = r.snapshot().map_err(|_| VmError::Corrupt)?;
        fold(&bytes);
        r = Runtime::restore(&bytes, &host).map_err(|_| VmError::Corrupt)?;
        match outcome {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            _ => return Err(VmError::Corrupt),
        }
    }
    if fs.open_count() != 1 || process.commands() != [b"mock command".to_vec()] {
        return Err(VmError::Corrupt);
    }
    r.begin_close()?;
    loop {
        let outcome = r.run(7, &mut j)?;
        let bytes = r.snapshot().map_err(|_| VmError::Corrupt)?;
        fold(&bytes);
        r = Runtime::restore(&bytes, &host).map_err(|_| VmError::Corrupt)?;
        match outcome {
            StepOutcome::Paused(_) => {}
            StepOutcome::Completed => break,
            _ => return Err(VmError::Corrupt),
        }
    }
    if fs.open_count() != 0 {
        return Err(VmError::Corrupt);
    }
    fold(&fs.contents(b"data").ok_or(VmError::Corrupt)?);
    fold(&r.fuel_consumed().to_le_bytes());
    for record in j.entries() {
        fold(&record.id.sequence.to_le_bytes());
        fold(record.request.as_deref().unwrap_or_default());
        fold(record.bytes.as_deref().unwrap_or_default());
    }
    Ok(hash)
}
