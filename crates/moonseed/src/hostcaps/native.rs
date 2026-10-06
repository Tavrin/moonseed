//! The crate's sole ambient-OS boundary. Path rooting checks every component
//! using symlink_metadata, and denies symlinks by default. These checks and
//! subsequent opens/renames are separate OS operations: concurrent namespace
//! changes can race them (TOCTOU). This is not directory-handle confinement;
//! use the VFS for strong confinement. Native resources refuse snapshots.

// Portable unit tests compile this private module for filesystem proofs and
// oracle launch helpers; the feature-gated adapter constructor is absent.
#![cfg_attr(
    all(test, not(feature = "native-host")),
    allow(dead_code, unused_imports)
)]

use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Native profile authority and resource limits. Construction grants only the
/// selected authority; process/environment are disabled by default.
#[derive(Clone, Debug)]
pub struct NativeOptions {
    /// Grant shell/process execution and pipes (shell commands are unconfined).
    pub process: bool,
    /// Grant reads of the process environment.
    pub env: bool,
    /// Grant standard input/output/error access.
    pub stdio: bool,
    /// Grant wall and CPU clock observations.
    pub clock: bool,
    /// Allow symlinks; targets may escape the root. Default false.
    pub allow_symlinks: bool,
    /// Deny filesystem mutation.
    pub read_only: bool,
    /// Maximum simultaneously open files (temporary handles included).
    pub max_open_files: usize,
    /// Maximum bytes requested in one host operation.
    pub max_bytes_per_op: usize,
}
impl Default for NativeOptions {
    fn default() -> Self {
        Self {
            process: false,
            env: false,
            stdio: true,
            clock: true,
            allow_symlinks: false,
            read_only: false,
            max_open_files: 256,
            max_bytes_per_op: 64 * 1024,
        }
    }
}
/// OS entropy for exclusive temp names. Linux's read-only /proc UUID source
/// remains usable when a host sandbox mounts /dev with nodev. The UUID is
/// kernel-generated version 4 (122 random bits); never use time or PID seeds.
#[cfg(unix)]
fn temporary_random() -> Result<[u8; 16], HostIoError> {
    let mut random = [0u8; 16];
    match std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut random)) {
        Ok(()) => Ok(random),
        Err(original) => {
            #[cfg(target_os = "linux")]
            {
                let mut uuid = [0u8; 36];
                std::fs::File::open("/proc/sys/kernel/random/uuid")
                    .and_then(|mut f| f.read_exact(&mut uuid))
                    .map_err(|_| error(original))?;
                let mut digits = uuid.iter().copied().filter(|b| *b != b'-');
                for byte in &mut random {
                    let hex = |b: u8| match b {
                        b'0'..=b'9' => Some(b - b'0'),
                        b'a'..=b'f' => Some(b - b'a' + 10),
                        _ => None,
                    };
                    let hi = digits
                        .next()
                        .and_then(hex)
                        .ok_or_else(HostIoError::invalid)?;
                    let lo = digits
                        .next()
                        .and_then(hex)
                        .ok_or_else(HostIoError::invalid)?;
                    *byte = hi * 16 + lo;
                }
                if digits.next().is_some() || random[6] >> 4 != 4 || random[8] >> 6 != 2 {
                    return Err(HostIoError::invalid());
                }
                Ok(random)
            }
            #[cfg(not(target_os = "linux"))]
            Err(error(original))
        }
    }
}

fn error(e: std::io::Error) -> HostIoError {
    let kind = match e.kind() {
        std::io::ErrorKind::NotFound => HostIoErrorKind::NotFound,
        std::io::ErrorKind::PermissionDenied => HostIoErrorKind::PermissionDenied,
        std::io::ErrorKind::AlreadyExists => HostIoErrorKind::AlreadyExists,
        std::io::ErrorKind::IsADirectory => HostIoErrorKind::IsDirectory,
        std::io::ErrorKind::InvalidInput => HostIoErrorKind::InvalidInput,
        std::io::ErrorKind::Unsupported => HostIoErrorKind::Unsupported,
        _ => HostIoErrorKind::Other,
    };
    let code = e.raw_os_error();
    let mut message = e.to_string();
    if let Some(code) = code {
        let suffix = format!(" (os error {code})");
        if let Some(text) = message.strip_suffix(&suffix) {
            message = text.to_owned();
        }
    }
    let mut out = HostIoError::new(kind, message.into_bytes());
    if let Some(code) = code {
        out.code = code;
    }
    out
}
fn os_string(bytes: &[u8]) -> Result<std::ffi::OsString, HostIoError> {
    if bytes.contains(&0) {
        return Err(HostIoError::invalid());
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(std::ffi::OsString::from_vec(bytes.to_vec()))
    }
    #[cfg(not(unix))]
    {
        Ok(std::ffi::OsString::from(
            std::str::from_utf8(bytes).map_err(|_| HostIoError::invalid())?,
        ))
    }
}
fn os_bytes(value: std::ffi::OsString) -> Result<Vec<u8>, HostIoError> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(value.into_vec())
    }
    #[cfg(not(unix))]
    {
        value
            .into_string()
            .map(String::into_bytes)
            .map_err(|_| HostIoError::invalid())
    }
}
// Owning this argument closes a real file before any temp removal.
// Wasm std uses a non-Drop placeholder for unsupported filesystem handles.
fn close_file(_file: std::fs::File) {}

struct OpenFile {
    file: std::fs::File,
    mode: OpenMode,
    temp: Option<PathBuf>,
}
#[derive(Default)]
struct Files {
    next: u64,
    open: BTreeMap<ResourceId, OpenFile>,
}
/// Root-relative native filesystem with positional I/O and explicit resource limits.
/// Rejects absolute paths, parent components and (by default) per-component symlinks.
/// Root is canonicalized once; concurrent namespace changes can race validation
/// (TOCTOU). Native handles are host state and always use `HandlePolicy::Refuse`.
pub struct NativeFilesystem {
    root: PathBuf,
    options: NativeOptions,
    files: RefCell<Files>,
}
impl NativeFilesystem {
    /// Bind a real root directory. Does not create it or acquire any file handles.
    pub fn new(root: impl AsRef<Path>, options: NativeOptions) -> Result<Self, HostIoError> {
        let root = std::fs::canonicalize(root).map_err(error)?;
        if !root.is_dir() {
            return Err(HostIoError::invalid());
        }
        Ok(Self {
            root,
            options,
            files: RefCell::new(Files::default()),
        })
    }
    fn path(&self, bytes: &[u8], missing_last: bool) -> Result<PathBuf, HostIoError> {
        let path = PathBuf::from(os_string(bytes)?);
        if path.as_os_str().is_empty() || path.is_absolute() {
            return Err(HostIoError::denied());
        }
        let parts: Vec<_> = path.components().collect();
        let mut out = self.root.clone();
        for (i, c) in parts.iter().enumerate() {
            match c {
                Component::Normal(n) => out.push(n),
                Component::CurDir => continue,
                _ => return Err(HostIoError::denied()),
            };
            match std::fs::symlink_metadata(&out) {
                Ok(m) if m.file_type().is_symlink() && !self.options.allow_symlinks => {
                    return Err(HostIoError::denied());
                }
                Ok(_) => {}
                Err(e)
                    if missing_last
                        && i + 1 == parts.len()
                        && e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(error(e)),
            }
        }
        if out == self.root {
            return Err(HostIoError::new(
                HostIoErrorKind::IsDirectory,
                b"path is a directory".to_vec(),
            ));
        }
        Ok(out)
    }
    fn mutable(&self) -> Result<(), HostIoError> {
        if self.options.read_only {
            Err(HostIoError::denied())
        } else {
            Ok(())
        }
    }
    fn bounded(&self, n: usize) -> Result<(), HostIoError> {
        if n > self.options.max_bytes_per_op {
            Err(HostIoError::denied())
        } else {
            Ok(())
        }
    }
    fn room(&self) -> Result<ResourceId, HostIoError> {
        let s = self.files.borrow();
        if s.open.len() >= self.options.max_open_files {
            return Err(HostIoError::denied());
        }
        Ok(ResourceId(
            s.next.checked_add(1).ok_or_else(HostIoError::invalid)?,
        ))
    }
    fn insert(
        &self,
        id: ResourceId,
        file: std::fs::File,
        mode: OpenMode,
        temp: Option<PathBuf>,
    ) -> ResourceId {
        let mut s = self.files.borrow_mut();
        s.next = id.0;
        s.open.insert(id, OpenFile { file, mode, temp });
        id
    }
    fn temporary(&self) -> Result<(std::fs::File, PathBuf, Vec<u8>), HostIoError> {
        #[cfg(not(unix))]
        {
            self.mutable()?;
            Err(HostIoError::unsupported())
        }
        #[cfg(unix)]
        {
            self.mutable()?;
            for _ in 0..128 {
                // OS entropy, never timestamps/PIDs/counters. create_new is O_EXCL;
                // Unix mode 0600 is applied atomically at creation.
                let random = temporary_random()?;
                let name = format!(
                    ".moonseed-{}",
                    random
                        .iter()
                        .map(|n| format!("{n:02x}"))
                        .collect::<String>()
                )
                .into_bytes();
                let path = self.path(&name, true)?;
                let mut opts = std::fs::OpenOptions::new();
                opts.read(true).write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.mode(0o600);
                }
                match opts.open(&path) {
                    Ok(file) => return Ok((file, path, name)),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(error(e)),
                }
            }
            Err(HostIoError::new(
                HostIoErrorKind::AlreadyExists,
                b"temporary namespace exhausted".to_vec(),
            ))
        }
    }
}
impl Drop for NativeFilesystem {
    fn drop(&mut self) {
        for (_, f) in std::mem::take(&mut self.files.get_mut().open) {
            close_file(f.file);
            if let Some(path) = f.temp {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}
impl Filesystem for NativeFilesystem {
    fn probe_readable(&self, path: &[u8]) -> Completion<bool> {
        Completion::Ready((|| {
            let p = match self.path(path, false) {
                Ok(p) => p,
                Err(e) if e.kind == HostIoErrorKind::NotFound => return Ok(false),
                // fopen succeeds for directories on the Linux oracle platform.
                // path() refuses the root for handle acquisition, but probing it is safe.
                Err(e) if e.kind == HostIoErrorKind::IsDirectory => self.root.clone(),
                Err(e) => return Err(e),
            };
            match std::fs::File::open(p) {
                Ok(_file) => Ok(true),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                    ) =>
                {
                    Ok(false)
                }
                Err(e) => Err(error(e)),
            }
        })())
    }
    fn open(&self, path: &[u8], mode: OpenMode) -> Completion<ResourceId> {
        Completion::Ready((|| {
            if !mode.valid() {
                return Err(HostIoError::invalid());
            }
            if mode.write || mode.create || mode.truncate {
                self.mutable()?;
            }
            let id = self.room()?;
            let path = self.path(path, mode.create)?;
            let file = std::fs::OpenOptions::new()
                .read(mode.read)
                .write(mode.write)
                .append(mode.append)
                .create(mode.create)
                .truncate(mode.truncate)
                .open(path)
                .map_err(error)?;
            if file.metadata().map_err(error)?.is_dir() {
                return Err(HostIoError::new(
                    HostIoErrorKind::IsDirectory,
                    b"path is a directory".to_vec(),
                ));
            }
            Ok(self.insert(id, file, mode, None))
        })())
    }
    fn read_at(&self, id: ResourceId, offset: u64, max: usize) -> Completion<Vec<u8>> {
        Completion::Ready((|| {
            self.bounded(max)?;
            let mut files = self.files.borrow_mut();
            let f = files.open.get_mut(&id).ok_or_else(HostIoError::invalid)?;
            if !f.mode.read {
                return Err(HostIoError::denied());
            }
            let mut bytes = vec![0; max];
            #[cfg(unix)]
            let n = {
                use std::os::unix::fs::FileExt;
                f.file.read_at(&mut bytes, offset).map_err(error)?
            };
            #[cfg(not(unix))]
            let n = {
                use std::io::{Seek, SeekFrom};
                f.file.seek(SeekFrom::Start(offset)).map_err(error)?;
                f.file.read(&mut bytes).map_err(error)?
            };
            bytes.truncate(n);
            Ok(bytes)
        })())
    }
    fn write_at(&self, id: ResourceId, offset: u64, bytes: &[u8]) -> Completion<usize> {
        Completion::Ready((|| {
            self.mutable()?;
            self.bounded(bytes.len())?;
            let mut files = self.files.borrow_mut();
            let f = files.open.get_mut(&id).ok_or_else(HostIoError::invalid)?;
            if !f.mode.write {
                return Err(HostIoError::denied());
            }
            if f.mode.append {
                return Err(HostIoError::invalid());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileExt;
                f.file.write_at(bytes, offset).map_err(error)
            }
            #[cfg(not(unix))]
            {
                use std::io::{Seek, SeekFrom};
                f.file.seek(SeekFrom::Start(offset)).map_err(error)?;
                f.file.write(bytes).map_err(error)
            }
        })())
    }
    fn append(&self, id: ResourceId, bytes: &[u8]) -> Completion<u64> {
        Completion::Ready((|| {
            self.mutable()?;
            self.bounded(bytes.len())?;
            let mut files = self.files.borrow_mut();
            let f = files.open.get_mut(&id).ok_or_else(HostIoError::invalid)?;
            if !f.mode.write || !f.mode.append {
                return Err(HostIoError::invalid());
            }
            f.file.write_all(bytes).map_err(error)?;
            Ok(f.file.metadata().map_err(error)?.len())
        })())
    }
    fn size(&self, id: ResourceId) -> Completion<u64> {
        Completion::Ready((|| {
            let s = self.files.borrow();
            Ok(s.open
                .get(&id)
                .ok_or_else(HostIoError::invalid)?
                .file
                .metadata()
                .map_err(error)?
                .len())
        })())
    }
    fn flush(&self, id: ResourceId) -> Completion<()> {
        Completion::Ready((|| {
            let mut s = self.files.borrow_mut();
            s.open
                .get_mut(&id)
                .ok_or_else(HostIoError::invalid)?
                .file
                .flush()
                .map_err(error)
        })())
    }
    fn close(&self, id: ResourceId) -> Completion<()> {
        Completion::Ready((|| {
            let f = self
                .files
                .borrow_mut()
                .open
                .remove(&id)
                .ok_or_else(HostIoError::invalid)?;
            close_file(f.file);
            if let Some(p) = f.temp {
                std::fs::remove_file(p).map_err(error)?;
            }
            Ok(())
        })())
    }
    fn remove(&self, path: &[u8]) -> Completion<()> {
        Completion::Ready((|| {
            self.mutable()?;
            std::fs::remove_file(self.path(path, false)?).map_err(error)
        })())
    }
    fn rename(&self, from: &[u8], to: &[u8]) -> Completion<()> {
        Completion::Ready((|| {
            self.mutable()?;
            std::fs::rename(self.path(from, false)?, self.path(to, true)?).map_err(error)
        })())
    }
    fn temp_file(&self) -> Completion<ResourceId> {
        Completion::Ready((|| {
            let id = self.room()?;
            let (file, path, _) = self.temporary()?;
            Ok(self.insert(id, file, OpenMode::parse(b"w+")?, Some(path)))
        })())
    }
    fn temp_name(&self) -> Completion<Vec<u8>> {
        Completion::Ready(self.temporary().map(|(file, _, name)| {
            close_file(file);
            name
        }))
    }
    fn read_file(&self, path: &[u8], max: usize) -> Completion<Vec<u8>> {
        Completion::Ready((|| {
            self.bounded(max)?;
            let f = std::fs::File::open(self.path(path, false)?).map_err(error)?;
            if !f.metadata().map_err(error)?.is_file() {
                return Err(HostIoError::new(
                    HostIoErrorKind::IsDirectory,
                    b"path is not a regular file".to_vec(),
                ));
            }
            let mut bytes = Vec::new();
            f.take(
                (max as u64)
                    .checked_add(1)
                    .ok_or_else(HostIoError::invalid)?,
            )
            .read_to_end(&mut bytes)
            .map_err(error)?;
            if bytes.len() > max {
                return Err(HostIoError::denied());
            }
            Ok(bytes)
        })())
    }
    fn read_file_range(&self, path: &[u8], offset: u64, max: usize) -> Completion<Vec<u8>> {
        Completion::Ready((|| {
            use std::io::{Seek, SeekFrom};
            self.bounded(max)?;
            let mut f = std::fs::File::open(self.path(path, false)?).map_err(error)?;
            if !f.metadata().map_err(error)?.is_file() {
                return Err(HostIoError::new(
                    HostIoErrorKind::IsDirectory,
                    b"path is not a regular file".to_vec(),
                ));
            }
            f.seek(SeekFrom::Start(offset)).map_err(error)?;
            let mut bytes = vec![0; max];
            // Fill the bounded range: an OS short read need not be EOF.
            let mut n = 0;
            while n < max {
                let got = f.read(&mut bytes[n..]).map_err(error)?;
                if got == 0 {
                    break;
                }
                n += got;
            }
            bytes.truncate(n);
            Ok(bytes)
        })())
    }
    fn handle_policy(&self) -> HandlePolicy {
        HandlePolicy::Refuse
    }
}
struct NativeStdio {
    max: usize,
}
impl Stdio for NativeStdio {
    fn read_stdin(&self, max: usize) -> Completion<Vec<u8>> {
        Completion::Ready((|| {
            if max > self.max {
                return Err(HostIoError::denied());
            }
            let mut b = vec![0; max];
            let n = std::io::stdin().lock().read(&mut b).map_err(error)?;
            b.truncate(n);
            Ok(b)
        })())
    }
    fn write_stdout(&self, bytes: &[u8]) -> Completion<usize> {
        Completion::Ready(if bytes.len() > self.max {
            Err(HostIoError::denied())
        } else {
            std::io::stdout().lock().write(bytes).map_err(error)
        })
    }
    fn write_stderr(&self, bytes: &[u8]) -> Completion<usize> {
        Completion::Ready(if bytes.len() > self.max {
            Err(HostIoError::denied())
        } else {
            std::io::stderr().lock().write(bytes).map_err(error)
        })
    }
    fn flush(&self, stream: Stream) -> Completion<()> {
        Completion::Ready(match stream {
            Stream::Stdout => std::io::stdout().lock().flush().map_err(error),
            Stream::Stderr => std::io::stderr().lock().flush().map_err(error),
            Stream::Stdin => Ok(()),
        })
    }
}
struct NativeEnvironment;
impl Environment for NativeEnvironment {
    fn get(&self, name: &[u8]) -> Completion<Option<Vec<u8>>> {
        Completion::Ready((|| {
            if name.is_empty() || name.contains(&b'=') {
                return Err(HostIoError::invalid());
            }
            std::env::var_os(os_string(name)?).map(os_bytes).transpose()
        })())
    }
}
struct NativeClock;
impl Clock for NativeClock {
    fn now_seconds(&self) -> Completion<i64> {
        Completion::Ready(
            match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
                Ok(d) => i64::try_from(d.as_secs()).map_err(|_| HostIoError::invalid()),
                Err(e) => i64::try_from(e.duration().as_secs())
                    .map(|n| -n - i64::from(e.duration().subsec_nanos() != 0))
                    .map_err(|_| HostIoError::invalid()),
            },
        )
    }
    fn cpu_seconds(&self) -> Completion<f64> {
        // procfs reports process CPU ticks and the kernel tick rate, not wall time.
        #[cfg(target_os = "linux")]
        {
            Completion::Ready((|| {
                let aux = std::fs::read("/proc/self/auxv").map_err(error)?;
                let width = std::mem::size_of::<usize>();
                let rate = aux
                    .chunks_exact(2 * width)
                    .find_map(|pair| {
                        let key = usize::from_ne_bytes(pair[..width].try_into().ok()?);
                        let value = usize::from_ne_bytes(pair[width..].try_into().ok()?);
                        (key == 17 && value > 0).then_some(value as f64)
                    })
                    .ok_or_else(HostIoError::unsupported)?;
                let stat = std::fs::read_to_string("/proc/self/stat").map_err(error)?;
                let (_, tail) = stat.rsplit_once(')').ok_or_else(HostIoError::invalid)?;
                let fields: Vec<_> = tail.split_whitespace().collect();
                let ticks = |i: usize| {
                    fields
                        .get(i)
                        .and_then(|s| s.parse::<u64>().ok())
                        .ok_or_else(HostIoError::invalid)
                };
                Ok((ticks(11)? as f64 + ticks(12)? as f64) / rate)
            })())
        }
        #[cfg(not(target_os = "linux"))]
        {
            Completion::Ready(Err(HostIoError::unsupported()))
        }
    }
}
struct Pipe {
    child: std::process::Child,
    mode: PipeMode,
}
struct NativeProcess {
    pipes: RefCell<BTreeMap<ResourceId, Pipe>>,
    next: RefCell<u64>,
    max: usize,
    max_open: usize,
}
fn shell(cmd: &[u8]) -> Result<std::process::Command, HostIoError> {
    #[cfg(unix)]
    {
        let mut c = std::process::Command::new("/bin/sh");
        c.arg("-c").arg(os_string(cmd)?);
        Ok(c)
    }
    #[cfg(not(unix))]
    {
        let mut c = std::process::Command::new("cmd.exe");
        c.arg("/C").arg(os_string(cmd)?);
        Ok(c)
    }
}
fn status(s: std::process::ExitStatus) -> Result<ProcessStatus, HostIoError> {
    if let Some(code) = s.code() {
        return Ok(ProcessStatus::Exit(code));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(n) = s.signal() {
            return Ok(ProcessStatus::Signal(n));
        }
    }
    Err(HostIoError::new(
        HostIoErrorKind::Other,
        b"unknown process status".to_vec(),
    ))
}
impl Process for NativeProcess {
    fn shell_available(&self) -> Completion<bool> {
        #[cfg(unix)]
        {
            Completion::Ready(Ok(std::fs::metadata("/bin/sh").is_ok()))
        }
        #[cfg(not(unix))]
        {
            Completion::Ready(Ok(true))
        }
    }
    fn execute(&self, cmd: &[u8]) -> Completion<ProcessStatus> {
        Completion::Ready((|| status(shell(cmd)?.status().map_err(error)?))())
    }
    fn popen(&self, cmd: &[u8], mode: PipeMode) -> Completion<ResourceId> {
        Completion::Ready((|| {
            if self.pipes.borrow().len() >= self.max_open {
                return Err(HostIoError::denied());
            }
            let id = ResourceId(
                self.next
                    .borrow()
                    .checked_add(1)
                    .ok_or_else(HostIoError::invalid)?,
            );
            let mut command = shell(cmd)?;
            match mode {
                PipeMode::Read => {
                    command.stdout(std::process::Stdio::piped());
                }
                PipeMode::Write => {
                    command.stdin(std::process::Stdio::piped());
                }
            }
            let child = command.spawn().map_err(error)?;
            *self.next.borrow_mut() = id.0;
            self.pipes.borrow_mut().insert(id, Pipe { child, mode });
            Ok(id)
        })())
    }
    fn read_at(&self, id: ResourceId, _offset: u64, max: usize) -> Completion<Vec<u8>> {
        Completion::Ready((|| {
            if max > self.max {
                return Err(HostIoError::denied());
            }
            let mut s = self.pipes.borrow_mut();
            let p = s.get_mut(&id).ok_or_else(HostIoError::invalid)?;
            if p.mode != PipeMode::Read {
                return Err(HostIoError::invalid());
            }
            let mut bytes = vec![0; max];
            let n = p
                .child
                .stdout
                .as_mut()
                .ok_or_else(HostIoError::invalid)?
                .read(&mut bytes)
                .map_err(error)?;
            bytes.truncate(n);
            Ok(bytes)
        })())
    }
    fn write_at(&self, id: ResourceId, _offset: u64, bytes: &[u8]) -> Completion<usize> {
        Completion::Ready((|| {
            if bytes.len() > self.max {
                return Err(HostIoError::denied());
            }
            let mut s = self.pipes.borrow_mut();
            let p = s.get_mut(&id).ok_or_else(HostIoError::invalid)?;
            if p.mode != PipeMode::Write {
                return Err(HostIoError::invalid());
            }
            p.child
                .stdin
                .as_mut()
                .ok_or_else(HostIoError::invalid)?
                .write(bytes)
                .map_err(error)
        })())
    }
    fn flush(&self, id: ResourceId) -> Completion<()> {
        Completion::Ready((|| {
            let mut s = self.pipes.borrow_mut();
            let p = s.get_mut(&id).ok_or_else(HostIoError::invalid)?;
            if let Some(stdin) = p.child.stdin.as_mut() {
                stdin.flush().map_err(error)?;
            }
            Ok(())
        })())
    }
    fn close(&self, id: ResourceId) -> Completion<ProcessStatus> {
        Completion::Ready((|| {
            let mut p = self
                .pipes
                .borrow_mut()
                .remove(&id)
                .ok_or_else(HostIoError::invalid)?;
            p.child.stdin.take();
            p.child.stdout.take();
            status(p.child.wait().map_err(error)?)
        })())
    }
}
impl Drop for NativeProcess {
    fn drop(&mut self) {
        for (_, mut p) in std::mem::take(self.pipes.get_mut()) {
            let _ = p.child.kill();
            let _ = p.child.wait();
        }
    }
}
/// Construct selected native authority. Filesystem paths are rooted, but shell
/// commands and environment access (when enabled) are process-wide authority.
/// Local civil conversion is absent: consumers use UTC by default. CPU-clock
/// observations use Linux procfs; unsupported platforms return structured errors.
/// Temp allocation needs Unix OS entropy; other platforms explicitly refuse it.
#[cfg(feature = "native-host")]
pub fn native_host(
    root: impl AsRef<Path>,
    options: NativeOptions,
) -> Result<crate::HostCapabilities, HostIoError> {
    let fs = NativeFilesystem::new(root, options.clone())?;
    let mut caps = crate::HostCapabilities::sandbox().filesystem(Arc::new(fs));
    if options.stdio {
        caps = caps.stdio(Arc::new(NativeStdio {
            max: options.max_bytes_per_op,
        }));
    }
    if options.clock {
        caps = caps.clock(Arc::new(NativeClock));
    }
    if options.env {
        caps = caps.environment(Arc::new(NativeEnvironment));
    }
    if options.process {
        caps = caps.process(Arc::new(NativeProcess {
            pipes: RefCell::new(BTreeMap::new()),
            next: RefCell::new(0),
            max: options.max_bytes_per_op,
            max_open: options.max_open_files,
        }));
    }
    Ok(caps)
}

// Measurement and historical proof harnesses also keep ambient calls here.
#[cfg(test)]
pub(crate) use std::time::Duration;
#[cfg(any(test, feature = "__measure"))]
pub(crate) use std::time::Instant;
#[cfg(test)]
pub(crate) mod test_support {
    pub(crate) use std::env::{temp_dir, var};
    pub(crate) use std::fs::{
        create_dir_all, read, read_dir, read_to_string, remove_dir_all, write,
    };
    pub(crate) use std::process::{Command, id};
}

#[cfg(feature = "__measure")]
pub(crate) fn measurement_env(name: &str) -> Result<String, std::env::VarError> {
    std::env::var(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ready<T>(c: Completion<T>) -> Result<T, HostIoError> {
        match c {
            Completion::Ready(r) => r,
            Completion::Pending(_) => panic!("native wait"),
        }
    }
    struct TempRoot(PathBuf);
    impl TempRoot {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "moonseed-c1-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempRoot {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    #[test]
    #[cfg(unix)]
    fn review_native_byte_paths_intermediate_symlinks_and_drop() {
        use std::os::unix::ffi::OsStrExt;
        let root = TempRoot::new();
        std::fs::create_dir(root.0.join("inside")).unwrap();
        std::fs::create_dir(root.0.join("outside")).unwrap();
        std::fs::write(root.0.join("outside/sentinel"), b"outside").unwrap();
        let fsroot = root.0.join("inside");
        std::os::unix::fs::symlink(root.0.join("outside"), fsroot.join("link")).unwrap();
        let fs = NativeFilesystem::new(&fsroot, NativeOptions::default()).unwrap();
        for path in [
            b"link/sentinel".as_slice(),
            b"../outside/sentinel",
            b"/absolute",
            b"bad\0path",
        ] {
            assert!(ready(fs.open(path, OpenMode::parse(b"r").unwrap())).is_err());
            assert!(ready(fs.read_file_range(path, 0, 64)).is_err());
        }
        assert!(ready(fs.remove(b"link/sentinel")).is_err());
        assert!(ready(fs.rename(b"link/sentinel", b"renamed")).is_err());
        for path in [b"f\xff".as_slice(), br"..\outside\sentinel"] {
            let id = ready(fs.open(path, OpenMode::parse(b"w+").unwrap())).unwrap();
            ready(fs.write_at(id, 0, b"confined")).unwrap();
            ready(fs.close(id)).unwrap();
            assert_eq!(
                std::fs::read(fsroot.join(std::ffi::OsStr::from_bytes(path))).unwrap(),
                b"confined"
            );
        }
        let temp = ready(fs.temp_file()).unwrap();
        ready(fs.write_at(temp, 0, b"temporary")).unwrap();
        assert!(
            std::fs::read_dir(&fsroot).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .as_bytes()
                .starts_with(b".moonseed-"))
        );
        drop(fs);
        assert!(
            !std::fs::read_dir(&fsroot).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .as_bytes()
                .starts_with(b".moonseed-"))
        );
        assert_eq!(
            std::fs::read(root.0.join("outside/sentinel")).unwrap(),
            b"outside"
        );
    }
    #[test]
    fn native_root_positional_temp_remove_rename_limits_and_symlinks() {
        let root = TempRoot::new();
        let fs = NativeFilesystem::new(
            &root.0,
            NativeOptions {
                max_open_files: 1,
                max_bytes_per_op: 4,
                ..NativeOptions::default()
            },
        )
        .unwrap();
        assert_eq!(fs.handle_policy(), HandlePolicy::Refuse);
        for path in [
            b"/etc/passwd".as_slice(),
            b"../escape",
            b"a/../../escape",
            b"x\0y",
        ] {
            assert!(ready(fs.open(path, OpenMode::parse(b"w").unwrap())).is_err());
        }
        let id = ready(fs.open(b"file", OpenMode::parse(b"w+").unwrap())).unwrap();
        assert_eq!(ready(fs.write_at(id, 2, b"ab")).unwrap(), 2);
        assert_eq!(ready(fs.read_at(id, 0, 4)).unwrap(), b"\0\0ab");
        assert_eq!(ready(fs.read_at(id, 2, 2)).unwrap(), b"ab");
        assert_eq!(ready(fs.size(id)).unwrap(), 4);
        assert!(ready(fs.open(b"other", OpenMode::parse(b"w").unwrap())).is_err());
        assert!(!root.0.join("other").exists());
        assert!(ready(fs.write_at(id, 0, b"12345")).is_err());
        assert!(ready(fs.read_at(id, 0, 5)).is_err());
        ready(fs.flush(id)).unwrap();
        ready(fs.close(id)).unwrap();
        assert!(ready(fs.close(id)).is_err());
        assert!(fs.rebind(id).is_err());
        let id = ready(fs.open(b"file", OpenMode::parse(b"a+").unwrap())).unwrap();
        assert_eq!(ready(fs.append(id, b"z")).unwrap(), 5);
        assert!(ready(fs.write_at(id, 0, b"x")).is_err());
        ready(fs.close(id)).unwrap();
        ready(fs.rename(b"file", b"renamed")).unwrap();
        assert!(ready(fs.read_file(b"renamed", 4)).is_err());
        ready(fs.remove(b"renamed")).unwrap();
        assert!(!ready(fs.probe_readable(b"renamed")).unwrap());
        let id = ready(fs.temp_file()).unwrap();
        ready(fs.write_at(id, 0, b"temp")).unwrap();
        assert_eq!(ready(fs.read_at(id, 0, 4)).unwrap(), b"temp");
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
        ready(fs.close(id)).unwrap();
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
        let a = ready(fs.temp_name()).unwrap();
        let b = ready(fs.temp_name()).unwrap();
        assert_ne!(a, b);
        assert!(root.0.join(os_string(&a).unwrap()).exists());
        assert!(root.0.join(os_string(&b).unwrap()).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            assert_eq!(
                std::fs::metadata(root.0.join(os_string(&a).unwrap()))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            std::fs::create_dir(root.0.join("dir")).unwrap();
            std::fs::write(root.0.join("dir/data"), b"x").unwrap();
            symlink("dir", root.0.join("link")).unwrap();
            symlink("dir/data", root.0.join("file-link")).unwrap();
            symlink("absent", root.0.join("dangling")).unwrap();
            for path in [b"link/data".as_slice(), b"file-link", b"dangling"] {
                assert!(ready(fs.open(path, OpenMode::parse(b"w+").unwrap())).is_err());
                assert!(ready(fs.read_file(path, 4)).is_err());
                assert!(ready(fs.remove(path)).is_err());
                assert!(ready(fs.rename(path, b"dest")).is_err());
                assert!(ready(fs.rename(&a, path)).is_err());
            }
            let id = ready(fs.open(b"\xff", OpenMode::parse(b"w").unwrap())).unwrap();
            ready(fs.close(id)).unwrap();
            ready(fs.remove(b"\xff")).unwrap();
        }
        let ro = NativeFilesystem::new(
            &root.0,
            NativeOptions {
                read_only: true,
                ..NativeOptions::default()
            },
        )
        .unwrap();
        assert!(ready(ro.open(b"new", OpenMode::parse(b"w").unwrap())).is_err());
        assert!(ready(ro.remove(&a)).is_err());
        assert!(ready(ro.rename(&a, b"new")).is_err());
        assert!(ready(ro.temp_file()).is_err());
        assert!(ready(ro.temp_name()).is_err());
    }
    #[test]
    #[cfg(feature = "native-host")]
    fn lua_native_file_snapshot_refuses_live_and_restores_closed() {
        let root = TempRoot::new();
        std::fs::write(root.0.join("f"), b"data").unwrap();
        let caps = native_host(
            &root.0,
            NativeOptions {
                stdio: false,
                ..Default::default()
            },
        )
        .unwrap();
        let mut runtime = crate::Runtime::builder()
            .libraries(crate::Libraries::STANDARD)
            .capabilities(caps.clone())
            .build()
            .unwrap();
        runtime
            .load_main(&crate::compile(b"f=assert(io.open('f'))").unwrap())
            .unwrap();
        let mut journal = crate::Journal::new();
        assert_eq!(
            runtime.run_until_terminal(100, &mut journal).unwrap(),
            crate::StepOutcome::Completed
        );
        assert!(matches!(
            runtime.snapshot(),
            Err(crate::SnapshotError::NonPortableResource { .. })
        ));
        runtime
            .load_main(&crate::compile(b"f:close()").unwrap())
            .unwrap();
        assert_eq!(
            runtime.run_until_terminal(100, &mut journal).unwrap(),
            crate::StepOutcome::Completed
        );
        let mut registry = crate::HostRegistry::new();
        crate::register_standard(&mut registry);
        crate::register_io(&mut registry);
        crate::Runtime::restore(
            &runtime.snapshot().unwrap(),
            &crate::Host::new(registry).capabilities(caps),
        )
        .unwrap();
    }

    #[test]
    #[cfg(feature = "native-host")]
    fn native_profile_authority_is_explicit() {
        let root = TempRoot::new();
        let sandbox = native_host(
            &root.0,
            NativeOptions {
                stdio: false,
                clock: false,
                ..NativeOptions::default()
            },
        )
        .unwrap();
        assert!(sandbox.filesystem.is_some());
        assert!(sandbox.process.is_none());
        assert!(sandbox.environment.is_none());
        assert!(sandbox.stdio.is_none());
        assert!(sandbox.clock.is_none());
        let full = native_host(
            &root.0,
            NativeOptions {
                process: true,
                env: true,
                ..NativeOptions::default()
            },
        )
        .unwrap();
        assert!(full.process.is_some());
        assert!(full.environment.is_some());
        assert!(full.clock.is_some());
        assert!(ready(full.environment.unwrap().get(b"bad\0name")).is_err());
    }
}
