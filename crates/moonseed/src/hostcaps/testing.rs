//! Deterministic mock authorities, available in CI/Wasm and to embedders without
//! enabling native-host. Clones share their mutable observations and counters.
use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// Fixed wall/CPU clock; never reads an OS clock.
#[derive(Clone, Debug)]
pub struct FixedClock {
    /// UTC epoch seconds.
    pub now: i64,
    /// CPU seconds.
    pub cpu: f64,
}
impl Clock for FixedClock {
    fn now_seconds(&self) -> Completion<i64> {
        Completion::Ready(Ok(self.now))
    }
    fn cpu_seconds(&self) -> Completion<f64> {
        Completion::Ready(Ok(self.cpu))
    }
}
/// Fixed local offset, with deterministic inverse conversion.
#[derive(Clone, Debug)]
pub struct FixedCivilTime {
    /// Timezone offset and DST observation.
    pub offset: CivilOffset,
}
impl CivilTime for FixedCivilTime {
    fn local_offset(&self, _utc_seconds: i64) -> Completion<CivilOffset> {
        Completion::Ready(Ok(self.offset))
    }
    fn utc_seconds(&self, local_seconds: i64, _isdst: Option<bool>) -> Completion<i64> {
        Completion::Ready(
            local_seconds
                .checked_sub(i64::from(self.offset.seconds))
                .ok_or_else(HostIoError::invalid),
        )
    }
}
/// Mutable byte-oriented environment map (NUL and non-UTF-8 are supported).
#[derive(Clone, Default)]
pub struct MemoryEnvironment {
    values: Rc<RefCell<BTreeMap<Vec<u8>, Vec<u8>>>>,
}
impl MemoryEnvironment {
    /// Create an empty environment.
    pub fn new() -> Self {
        Self::default()
    }
    /// Set an exact byte name and value.
    pub fn set(&self, name: Vec<u8>, value: Vec<u8>) {
        self.values.borrow_mut().insert(name, value);
    }
    /// Remove an exact byte name.
    pub fn remove(&self, name: &[u8]) {
        self.values.borrow_mut().remove(name);
    }
}
impl Environment for MemoryEnvironment {
    fn get(&self, name: &[u8]) -> Completion<Option<Vec<u8>>> {
        Completion::Ready(Ok(self.values.borrow().get(name).cloned()))
    }
}
#[derive(Default)]
struct Streams {
    input: Vec<u8>,
    cursor: usize,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    flushes: Vec<Stream>,
}
/// Sequential in-memory standard streams with inspectable writes and flushes.
#[derive(Clone, Default)]
pub struct MemoryStdio {
    state: Rc<RefCell<Streams>>,
}
impl MemoryStdio {
    /// Seed stdin bytes; outputs start empty.
    pub fn new(input: Vec<u8>) -> Self {
        Self {
            state: Rc::new(RefCell::new(Streams {
                input,
                ..Streams::default()
            })),
        }
    }
    /// Copy stdout.
    pub fn stdout(&self) -> Vec<u8> {
        self.state.borrow().stdout.clone()
    }
    /// Copy stderr.
    pub fn stderr(&self) -> Vec<u8> {
        self.state.borrow().stderr.clone()
    }
    /// Copy semantic flush observations.
    pub fn flushes(&self) -> Vec<Stream> {
        self.state.borrow().flushes.clone()
    }
}
impl Stdio for MemoryStdio {
    fn read_stdin(&self, max: usize) -> Completion<Vec<u8>> {
        let mut s = self.state.borrow_mut();
        let end = s.cursor.saturating_add(max).min(s.input.len());
        let bytes = s.input[s.cursor..end].to_vec();
        s.cursor = end;
        Completion::Ready(Ok(bytes))
    }
    fn write_stdout(&self, bytes: &[u8]) -> Completion<usize> {
        self.state.borrow_mut().stdout.extend(bytes);
        Completion::Ready(Ok(bytes.len()))
    }
    fn write_stderr(&self, bytes: &[u8]) -> Completion<usize> {
        self.state.borrow_mut().stderr.extend(bytes);
        Completion::Ready(Ok(bytes.len()))
    }
    fn flush(&self, stream: Stream) -> Completion<()> {
        self.state.borrow_mut().flushes.push(stream);
        Completion::Ready(Ok(()))
    }
}
/// Mock process executor: returns a fixed status and records exact commands.
/// Pipes explicitly return Unsupported rather than spawning a process.
#[derive(Clone)]
pub struct MockProcess {
    status: ProcessStatus,
    commands: Rc<RefCell<Vec<Vec<u8>>>>,
}
impl MockProcess {
    /// Set the status of every execution.
    pub fn new(status: ProcessStatus) -> Self {
        Self {
            status,
            commands: Rc::new(RefCell::new(Vec::new())),
        }
    }
    /// Copy executed commands (for exactly-once witnesses).
    pub fn commands(&self) -> Vec<Vec<u8>> {
        self.commands.borrow().clone()
    }
}
impl Process for MockProcess {
    fn shell_available(&self) -> Completion<bool> {
        Completion::Ready(Ok(true))
    }
    fn execute(&self, cmd: &[u8]) -> Completion<ProcessStatus> {
        self.commands.borrow_mut().push(cmd.to_vec());
        Completion::Ready(Ok(self.status))
    }
}
/// An authority that returns Pending for every operation in all six traits.
/// It records invocation names and monotonically numbered host tokens; complete
/// each VM wait explicitly with a correctly typed outcome. No operation is run
/// by this mock. Implementations below deliberately cover every waitable method.
#[derive(Clone, Default)]
pub struct PendingHost {
    calls: Rc<RefCell<Vec<&'static str>>>,
}
impl PendingHost {
    /// Create an empty invocation log.
    pub fn new() -> Self {
        Self::default()
    }
    /// Copy invocation names, in order.
    pub fn calls(&self) -> Vec<&'static str> {
        self.calls.borrow().clone()
    }
    fn pending<T>(&self, name: &'static str) -> Completion<T> {
        let mut calls = self.calls.borrow_mut();
        calls.push(name);
        Completion::Pending(PendingToken(calls.len() as u64))
    }
}
impl Filesystem for PendingHost {
    fn probe_readable(&self, path: &[u8]) -> Completion<bool> {
        let _ = path;
        self.pending("Filesystem.probe_readable")
    }
    fn open(&self, path: &[u8], mode: OpenMode) -> Completion<ResourceId> {
        let _ = path;
        let _ = mode;
        self.pending("Filesystem.open")
    }
    fn read_at(&self, id: ResourceId, offset: u64, max: usize) -> Completion<Vec<u8>> {
        let _ = id;
        let _ = offset;
        let _ = max;
        self.pending("Filesystem.read_at")
    }
    fn write_at(&self, id: ResourceId, offset: u64, bytes: &[u8]) -> Completion<usize> {
        let _ = id;
        let _ = offset;
        let _ = bytes;
        self.pending("Filesystem.write_at")
    }
    fn append(&self, id: ResourceId, bytes: &[u8]) -> Completion<u64> {
        let _ = id;
        let _ = bytes;
        self.pending("Filesystem.append")
    }
    fn size(&self, id: ResourceId) -> Completion<u64> {
        let _ = id;
        self.pending("Filesystem.size")
    }
    fn flush(&self, id: ResourceId) -> Completion<()> {
        let _ = id;
        self.pending("Filesystem.flush")
    }
    fn close(&self, id: ResourceId) -> Completion<()> {
        let _ = id;
        self.pending("Filesystem.close")
    }
    fn remove(&self, path: &[u8]) -> Completion<()> {
        let _ = path;
        self.pending("Filesystem.remove")
    }
    fn rename(&self, from: &[u8], to: &[u8]) -> Completion<()> {
        let _ = from;
        let _ = to;
        self.pending("Filesystem.rename")
    }
    fn temp_file(&self) -> Completion<ResourceId> {
        self.pending("Filesystem.temp_file")
    }
    fn temp_name(&self) -> Completion<Vec<u8>> {
        self.pending("Filesystem.temp_name")
    }
    fn read_file_range(&self, _path: &[u8], _offset: u64, _max: usize) -> Completion<Vec<u8>> {
        self.pending("Filesystem.read_file_range")
    }
    fn read_file(&self, path: &[u8], max: usize) -> Completion<Vec<u8>> {
        let _ = path;
        let _ = max;
        self.pending("Filesystem.read_file")
    }
}
impl Stdio for PendingHost {
    fn read_stdin(&self, max: usize) -> Completion<Vec<u8>> {
        let _ = max;
        self.pending("Stdio.read_stdin")
    }
    fn write_stdout(&self, bytes: &[u8]) -> Completion<usize> {
        let _ = bytes;
        self.pending("Stdio.write_stdout")
    }
    fn write_stderr(&self, bytes: &[u8]) -> Completion<usize> {
        let _ = bytes;
        self.pending("Stdio.write_stderr")
    }
    fn flush(&self, stream: Stream) -> Completion<()> {
        let _ = stream;
        self.pending("Stdio.flush")
    }
}
impl Clock for PendingHost {
    fn now_seconds(&self) -> Completion<i64> {
        self.pending("Clock.now_seconds")
    }
    fn cpu_seconds(&self) -> Completion<f64> {
        self.pending("Clock.cpu_seconds")
    }
}
impl CivilTime for PendingHost {
    fn zone_name(&self, _utc_seconds: i64) -> Completion<Vec<u8>> {
        self.pending("CivilTime.zone_name")
    }
    fn local_offset(&self, utc_seconds: i64) -> Completion<CivilOffset> {
        let _ = utc_seconds;
        self.pending("CivilTime.local_offset")
    }
    fn utc_seconds(&self, local_seconds: i64, isdst: Option<bool>) -> Completion<i64> {
        let _ = local_seconds;
        let _ = isdst;
        self.pending("CivilTime.utc_seconds")
    }
}
impl Environment for PendingHost {
    fn get(&self, name: &[u8]) -> Completion<Option<Vec<u8>>> {
        let _ = name;
        self.pending("Environment.get")
    }
}
impl Process for PendingHost {
    fn shell_available(&self) -> Completion<bool> {
        self.pending("Process.shell_available")
    }
    fn execute(&self, cmd: &[u8]) -> Completion<ProcessStatus> {
        let _ = cmd;
        self.pending("Process.execute")
    }
    fn popen(&self, cmd: &[u8], mode: PipeMode) -> Completion<ResourceId> {
        let _ = cmd;
        let _ = mode;
        self.pending("Process.popen")
    }
    fn read_at(&self, id: ResourceId, offset: u64, max: usize) -> Completion<Vec<u8>> {
        let _ = id;
        let _ = offset;
        let _ = max;
        self.pending("Process.read_at")
    }
    fn write_at(&self, id: ResourceId, offset: u64, bytes: &[u8]) -> Completion<usize> {
        let _ = id;
        let _ = offset;
        let _ = bytes;
        self.pending("Process.write_at")
    }
    fn flush(&self, id: ResourceId) -> Completion<()> {
        let _ = id;
        self.pending("Process.flush")
    }
    fn close(&self, id: ResourceId) -> Completion<ProcessStatus> {
        let _ = id;
        self.pending("Process.close")
    }
}
