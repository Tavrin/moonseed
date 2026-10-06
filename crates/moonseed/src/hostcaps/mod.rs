//! Explicit host authority for filesystem, streams, time, environment and processes.
//!
//! These objects are host state, shared by construction and restore, never included
//! in a VM snapshot. Preserve the VFS object (or reconstruct its resource table)
//! when rebinding. A snapshot preserves VM history, not the future external world.
//! All calls go through `Runtime::capability`; embedders persist the journal.
//! `Completion<T>` here is the capability protocol; the existing root-level
//! `Completion` remains the Lua-native wait protocol for compatibility.

// RFC D1 requires shared Arc trait objects without Send/Sync constraints.
#![allow(clippy::arc_with_non_send_sync)]

mod memory;
#[cfg(any(feature = "native-host", test))]
pub(crate) mod native;
pub(crate) mod protocol;
/// Deterministic reference capabilities, available on every target without a feature.
pub mod testing;
pub use memory::{MemoryFilesystem, MemoryOptions, memory_filesystem};
#[cfg(feature = "native-host")]
pub use native::{NativeFilesystem, NativeOptions, native_host};
pub use protocol::{CapabilityPoll, CapabilityRequest, CapabilityValue, EffectClass};

/// A stable backend resource key; never a native file descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceId(pub u64);
/// An opaque host operation token, exposed in the pending wait payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingToken(pub u64);
/// A bounded operation either finishes or asks the host to complete it later.
#[derive(Clone, Debug, PartialEq)]
pub enum Completion<T> {
    /// Final success or structured failure.
    Ready(Result<T, HostIoError>),
    /// The host owns this in-flight operation; do not invoke it again.
    Pending(PendingToken),
}
impl<T> Completion<T> {
    pub(crate) fn map<U>(self, f: impl FnOnce(T) -> U) -> Completion<U> {
        match self {
            Self::Ready(r) => Completion::Ready(r.map(f)),
            Self::Pending(t) => Completion::Pending(t),
        }
    }
}
/// Portable filesystem/host error category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HostIoErrorKind {
    /// No entry exists.
    NotFound,
    /// Authority or configured limits deny the operation.
    PermissionDenied,
    /// A create-only entry already exists.
    AlreadyExists,
    /// A file operation named a directory.
    IsDirectory,
    /// Malformed or out-of-range input.
    InvalidInput,
    /// This backend does not implement the operation.
    Unsupported,
    /// Other host failure.
    Other,
}
/// Portable host failure; message bytes are not Rust debug text.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct HostIoError {
    /// Portable error category.
    pub kind: HostIoErrorKind,
    /// OS errno where available, otherwise a stable synthetic errno.
    pub code: i32,
    /// Human-readable diagnostic bytes.
    pub message: Vec<u8>,
}
impl HostIoError {
    /// Create an error with stable synthetic errno (2/13/17/21/22/38/5).
    pub fn new(kind: HostIoErrorKind, message: impl Into<Vec<u8>>) -> Self {
        let code = match kind {
            HostIoErrorKind::NotFound => 2,
            HostIoErrorKind::PermissionDenied => 13,
            HostIoErrorKind::AlreadyExists => 17,
            HostIoErrorKind::IsDirectory => 21,
            HostIoErrorKind::InvalidInput => 22,
            HostIoErrorKind::Unsupported => 38,
            HostIoErrorKind::Other => 5,
        };
        Self {
            kind,
            code,
            message: message.into(),
        }
    }
    pub(crate) fn unsupported() -> Self {
        Self::new(
            HostIoErrorKind::Unsupported,
            b"operation not supported".to_vec(),
        )
    }
    pub(crate) fn invalid() -> Self {
        Self::new(
            HostIoErrorKind::InvalidInput,
            b"invalid resource or input".to_vec(),
        )
    }
    pub(crate) fn denied() -> Self {
        Self::new(
            HostIoErrorKind::PermissionDenied,
            b"host access denied".to_vec(),
        )
    }
}
impl std::fmt::Display for HostIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.message))
    }
}
impl std::error::Error for HostIoError {}
/// Snapshot treatment for live backend handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandlePolicy {
    /// Restore explicitly reacquires the same resource key.
    Rebind,
    /// A reachable open handle prevents a snapshot.
    Refuse,
}
/// Validated Lua open mode; the VM owns the logical cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenMode {
    /// Permit reads.
    pub read: bool,
    /// Permit writes.
    pub write: bool,
    /// Writes append atomically at the backend's end.
    pub append: bool,
    /// Create a missing file.
    pub create: bool,
    /// Truncate on acquisition.
    pub truncate: bool,
    /// Binary hint; Moonseed never translates newlines.
    pub binary: bool,
}
impl OpenMode {
    /// Parse exactly r/w/a, optional + then optional b, or b then +.
    pub fn parse(bytes: &[u8]) -> Result<Self, HostIoError> {
        let Some((&first, tail)) = bytes.split_first() else {
            return Err(HostIoError::invalid());
        };
        if !matches!(first, b'r' | b'w' | b'a')
            || !matches!(tail, b"" | b"+" | b"b" | b"+b" | b"b+")
        {
            return Err(HostIoError::invalid());
        }
        Ok(Self {
            read: first == b'r' || tail.contains(&b'+'),
            write: first != b'r' || tail.contains(&b'+'),
            append: first == b'a',
            create: first != b'r',
            truncate: first == b'w',
            binary: tail.contains(&b'b'),
        })
    }
    pub(crate) fn valid(self) -> bool {
        (self.read || self.write)
            && (!self.append || (self.write && self.create && !self.truncate))
            && (!self.truncate || (self.write && self.create))
            && (!self.create || self.write)
    }
}
/// Standard stream identity, stable across restore.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    /// Standard input.
    Stdin,
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}
/// Process pipe direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipeMode {
    /// Read the child's stdout.
    Read,
    /// Write the child's stdin.
    Write,
}
/// Portable process termination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessStatus {
    /// Normal exit code.
    Exit(i32),
    /// Signal number on hosts that support signals.
    Signal(i32),
}
/// Local offset and daylight-saving state at one UTC instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CivilOffset {
    /// Seconds east of UTC.
    pub seconds: i32,
    /// Whether daylight saving is active.
    pub isdst: bool,
}
/// Independent filesystem authority. Defaults explicitly reject unsupported operations.
/// Byte paths are backend-specific. Reads/writes are positional; no backend cursor is observable.
pub trait Filesystem: 'static {
    /// Perform `probe_readable`; return final success/failure or an opaque pending token.
    fn probe_readable(&self, path: &[u8]) -> Completion<bool> {
        let _ = path;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `open`; return final success/failure or an opaque pending token.
    fn open(&self, path: &[u8], mode: OpenMode) -> Completion<ResourceId> {
        let _ = path;
        let _ = mode;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `read_at`; return final success/failure or an opaque pending token.
    fn read_at(&self, id: ResourceId, offset: u64, max: usize) -> Completion<Vec<u8>> {
        let _ = id;
        let _ = offset;
        let _ = max;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `write_at`; return final success/failure or an opaque pending token.
    fn write_at(&self, id: ResourceId, offset: u64, bytes: &[u8]) -> Completion<usize> {
        let _ = id;
        let _ = offset;
        let _ = bytes;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `append`; return final success/failure or an opaque pending token.
    fn append(&self, id: ResourceId, bytes: &[u8]) -> Completion<u64> {
        let _ = id;
        let _ = bytes;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `size`; return final success/failure or an opaque pending token.
    fn size(&self, id: ResourceId) -> Completion<u64> {
        let _ = id;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `flush`; return final success/failure or an opaque pending token.
    fn flush(&self, id: ResourceId) -> Completion<()> {
        let _ = id;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `close`; return final success/failure or an opaque pending token.
    fn close(&self, id: ResourceId) -> Completion<()> {
        let _ = id;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `remove`; return final success/failure or an opaque pending token.
    fn remove(&self, path: &[u8]) -> Completion<()> {
        let _ = path;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `rename`; return final success/failure or an opaque pending token.
    fn rename(&self, from: &[u8], to: &[u8]) -> Completion<()> {
        let _ = from;
        let _ = to;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `temp_file`; return final success/failure or an opaque pending token.
    fn temp_file(&self) -> Completion<ResourceId> {
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `temp_name`; return final success/failure or an opaque pending token.
    fn temp_name(&self) -> Completion<Vec<u8>> {
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `read_file`; return final success/failure or an opaque pending token.
    fn read_file(&self, path: &[u8], max: usize) -> Completion<Vec<u8>> {
        let _ = path;
        let _ = max;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Read a bounded range of a named file without retaining a live resource.
    /// Used by checkpointable source loading. Short results indicate EOF.
    /// The default supports only a whole file fitting in the first request.
    fn read_file_range(&self, path: &[u8], offset: u64, max: usize) -> Completion<Vec<u8>> {
        if offset == 0 {
            self.read_file(path, max)
        } else {
            Completion::Ready(Err(HostIoError::unsupported()))
        }
    }
    /// Snapshot policy for live resource keys.
    fn handle_policy(&self) -> HandlePolicy {
        HandlePolicy::Refuse
    }
    /// Reacquire an existing resource without creating, truncating or changing it.
    fn rebind(&self, id: ResourceId) -> Result<(), HostIoError> {
        let _ = id;
        Err(HostIoError::unsupported())
    }
}
// Policy is host code too, although it is metadata rather than an IO effect.
pub(crate) fn filesystem_policy(fs: &dyn Filesystem) -> Result<HandlePolicy, HostIoError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fs.handle_policy()))
        .map_err(|_| HostIoError::new(HostIoErrorKind::Other, b"host capability panicked".to_vec()))
}
/// Independent stdio authority. Defaults explicitly reject unsupported operations.
pub trait Stdio: 'static {
    /// Perform `read_stdin`; return final success/failure or an opaque pending token.
    fn read_stdin(&self, max: usize) -> Completion<Vec<u8>> {
        let _ = max;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `write_stdout`; return final success/failure or an opaque pending token.
    fn write_stdout(&self, bytes: &[u8]) -> Completion<usize> {
        let _ = bytes;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `write_stderr`; return final success/failure or an opaque pending token.
    fn write_stderr(&self, bytes: &[u8]) -> Completion<usize> {
        let _ = bytes;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `flush`; return final success/failure or an opaque pending token.
    fn flush(&self, stream: Stream) -> Completion<()> {
        let _ = stream;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
}
/// Independent clock authority. Defaults explicitly reject unsupported operations.
/// CPU seconds must measure CPU consumption, never elapsed wall time.
pub trait Clock: 'static {
    /// Perform `now_seconds`; return final success/failure or an opaque pending token.
    fn now_seconds(&self) -> Completion<i64> {
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `cpu_seconds`; return final success/failure or an opaque pending token.
    fn cpu_seconds(&self) -> Completion<f64> {
        Completion::Ready(Err(HostIoError::unsupported()))
    }
}
/// Independent civiltime authority. Defaults explicitly reject unsupported operations.
pub trait CivilTime: 'static {
    /// C-locale timezone abbreviation for `%Z` at this instant.
    /// The default is empty: an offset alone does not identify a timezone.
    fn zone_name(&self, _utc_seconds: i64) -> Completion<Vec<u8>> {
        Completion::Ready(Ok(Vec::new()))
    }

    /// Perform `local_offset`; return final success/failure or an opaque pending token.
    fn local_offset(&self, utc_seconds: i64) -> Completion<CivilOffset> {
        let _ = utc_seconds;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `utc_seconds`; return final success/failure or an opaque pending token.
    fn utc_seconds(&self, local_seconds: i64, isdst: Option<bool>) -> Completion<i64> {
        let _ = local_seconds;
        let _ = isdst;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
}
/// Independent environment authority. Defaults explicitly reject unsupported operations.
pub trait Environment: 'static {
    /// Perform `get`; return final success/failure or an opaque pending token.
    fn get(&self, name: &[u8]) -> Completion<Option<Vec<u8>>> {
        let _ = name;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
}
/// Independent process authority. Defaults explicitly reject unsupported operations.
pub trait Process: 'static {
    /// Perform `shell_available`; return final success/failure or an opaque pending token.
    fn shell_available(&self) -> Completion<bool> {
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `execute`; return final success/failure or an opaque pending token.
    fn execute(&self, cmd: &[u8]) -> Completion<ProcessStatus> {
        let _ = cmd;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `popen`; return final success/failure or an opaque pending token.
    fn popen(&self, cmd: &[u8], mode: PipeMode) -> Completion<ResourceId> {
        let _ = cmd;
        let _ = mode;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `read_at`; return final success/failure or an opaque pending token.
    fn read_at(&self, id: ResourceId, offset: u64, max: usize) -> Completion<Vec<u8>> {
        let _ = id;
        let _ = offset;
        let _ = max;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `write_at`; return final success/failure or an opaque pending token.
    fn write_at(&self, id: ResourceId, offset: u64, bytes: &[u8]) -> Completion<usize> {
        let _ = id;
        let _ = offset;
        let _ = bytes;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `flush`; return final success/failure or an opaque pending token.
    fn flush(&self, id: ResourceId) -> Completion<()> {
        let _ = id;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Perform `close`; return final success/failure or an opaque pending token.
    fn close(&self, id: ResourceId) -> Completion<ProcessStatus> {
        let _ = id;
        Completion::Ready(Err(HostIoError::unsupported()))
    }
    /// Snapshot policy for live resource keys.
    fn handle_policy(&self) -> HandlePolicy {
        HandlePolicy::Refuse
    }
    /// Reacquire an existing resource without creating, truncating or changing it.
    fn rebind(&self, id: ResourceId) -> Result<(), HostIoError> {
        let _ = id;
        Err(HostIoError::unsupported())
    }
}

#[cfg(test)]
mod tests;
