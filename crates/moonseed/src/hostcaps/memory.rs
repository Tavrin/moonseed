use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

/// Independent backend resource limits, separate from Lua heap/fuel limits.
#[derive(Clone, Debug)]
pub struct MemoryOptions {
    /// Maximum simultaneously open resources.
    pub max_open_files: usize,
    /// Maximum bytes requested by one read/write/whole-file operation.
    pub max_bytes_per_op: usize,
    /// Deny creation, writes, removal, rename and temporary reservations.
    pub read_only: bool,
    /// Maximum bytes in one file, including sparse writes.
    pub max_file_bytes: usize,
}
impl Default for MemoryOptions {
    fn default() -> Self {
        Self {
            max_open_files: 256,
            max_bytes_per_op: 64 * 1024,
            read_only: false,
            max_file_bytes: 16 * 1024 * 1024,
        }
    }
}
#[derive(Clone)]
struct OpenFile {
    data: Rc<RefCell<Vec<u8>>>,
    mode: OpenMode,
}
#[derive(Default)]
struct State {
    files: BTreeMap<Vec<u8>, Rc<RefCell<Vec<u8>>>>,
    open: BTreeMap<ResourceId, OpenFile>,
    next: u64,
    temp: u64,
}
/// Pure Rust, deterministic byte-path filesystem. Paths are exact opaque keys,
/// including NUL and invalid UTF-8; no directories or OS normalization exist.
/// Open files survive remove/rename. Temporary handles are anonymous and drop
/// their storage on close; temporary names reserve visible empty entries.
/// Clones share files/resources; preserve this object across snapshot restore.
#[derive(Clone)]
pub struct MemoryFilesystem {
    state: Rc<RefCell<State>>,
    options: MemoryOptions,
}
impl MemoryFilesystem {
    /// Seed initial files, bypassing read-only policy but enforcing file-size limits.
    /// Duplicate paths take the last supplied value.
    pub fn new(
        files: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>,
        options: MemoryOptions,
    ) -> Result<Self, HostIoError> {
        let mut state = State::default();
        for (path, data) in files {
            if data.len() > options.max_file_bytes {
                return Err(HostIoError::denied());
            }
            state.files.insert(path, Rc::new(RefCell::new(data)));
        }
        Ok(Self {
            state: Rc::new(RefCell::new(state)),
            options,
        })
    }
    /// Copy a named file without acquiring a handle; useful for reference fixtures.
    pub fn contents(&self, path: &[u8]) -> Option<Vec<u8>> {
        self.state
            .borrow()
            .files
            .get(path)
            .map(|d| d.borrow().clone())
    }
    /// Count live handles (temporary handles included).
    pub fn open_count(&self) -> usize {
        self.state.borrow().open.len()
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
    fn room(&self, s: &State) -> Result<ResourceId, HostIoError> {
        if s.open.len() >= self.options.max_open_files {
            return Err(HostIoError::denied());
        }
        Ok(ResourceId(
            s.next.checked_add(1).ok_or_else(HostIoError::invalid)?,
        ))
    }
    fn named(s: &State, path: &[u8]) -> Result<Rc<RefCell<Vec<u8>>>, HostIoError> {
        s.files
            .get(path)
            .cloned()
            .ok_or_else(|| HostIoError::new(HostIoErrorKind::NotFound, b"file not found".to_vec()))
    }
    fn handle(&self, id: ResourceId) -> Result<OpenFile, HostIoError> {
        self.state
            .borrow()
            .open
            .get(&id)
            .cloned()
            .ok_or_else(HostIoError::invalid)
    }
    fn write(&self, file: &OpenFile, offset: u64, bytes: &[u8]) -> Result<usize, HostIoError> {
        self.mutable()?;
        self.bounded(bytes.len())?;
        if !file.mode.write {
            return Err(HostIoError::denied());
        }
        let start = usize::try_from(offset).map_err(|_| HostIoError::invalid())?;
        let end = start
            .checked_add(bytes.len())
            .ok_or_else(HostIoError::invalid)?;
        if end > self.options.max_file_bytes {
            return Err(HostIoError::denied());
        }
        // Empty writes never enlarge a file.
        if bytes.is_empty() {
            return Ok(0);
        }
        let mut data = file.data.borrow_mut();
        if end > data.len() {
            data.resize(end, 0);
        }
        data[start..end].copy_from_slice(bytes);
        Ok(bytes.len())
    }
}
/// Construct a profile with only the deterministic filesystem authority.
pub fn memory_filesystem(
    files: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>,
    options: MemoryOptions,
) -> Result<crate::HostCapabilities, HostIoError> {
    Ok(crate::HostCapabilities::sandbox()
        .filesystem(Arc::new(MemoryFilesystem::new(files, options)?)))
}
impl Filesystem for MemoryFilesystem {
    fn probe_readable(&self, path: &[u8]) -> Completion<bool> {
        Completion::Ready(Ok(self.state.borrow().files.contains_key(path)))
    }
    fn open(&self, path: &[u8], mode: OpenMode) -> Completion<ResourceId> {
        Completion::Ready((|| {
            if !mode.valid() {
                return Err(HostIoError::invalid());
            }
            if mode.write || mode.create || mode.truncate {
                self.mutable()?;
            }
            let mut s = self.state.borrow_mut();
            let id = self.room(&s)?;
            let data = match s.files.get(path) {
                Some(d) => d.clone(),
                None if mode.create => {
                    let d = Rc::new(RefCell::new(Vec::new()));
                    s.files.insert(path.to_vec(), d.clone());
                    d
                }
                None => {
                    return Err(HostIoError::new(
                        HostIoErrorKind::NotFound,
                        b"file not found".to_vec(),
                    ));
                }
            };
            if mode.truncate {
                data.borrow_mut().clear();
            }
            s.next = id.0;
            s.open.insert(id, OpenFile { data, mode });
            Ok(id)
        })())
    }
    fn read_at(&self, id: ResourceId, offset: u64, max: usize) -> Completion<Vec<u8>> {
        Completion::Ready((|| {
            self.bounded(max)?;
            let file = self.handle(id)?;
            if !file.mode.read {
                return Err(HostIoError::denied());
            }
            let data = file.data.borrow();
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(data.len());
            let end = start.saturating_add(max).min(data.len());
            Ok(data[start..end].to_vec())
        })())
    }
    fn write_at(&self, id: ResourceId, offset: u64, bytes: &[u8]) -> Completion<usize> {
        Completion::Ready((|| {
            let f = self.handle(id)?;
            if f.mode.append {
                return Err(HostIoError::invalid());
            }
            self.write(&f, offset, bytes)
        })())
    }
    fn append(&self, id: ResourceId, bytes: &[u8]) -> Completion<u64> {
        Completion::Ready((|| {
            let f = self.handle(id)?;
            if !f.mode.append {
                return Err(HostIoError::invalid());
            }
            let len = f.data.borrow().len() as u64;
            self.write(&f, len, bytes)?;
            Ok(f.data.borrow().len() as u64)
        })())
    }
    fn size(&self, id: ResourceId) -> Completion<u64> {
        Completion::Ready(self.handle(id).map(|f| f.data.borrow().len() as u64))
    }
    fn flush(&self, id: ResourceId) -> Completion<()> {
        Completion::Ready(self.handle(id).map(|_| ()))
    }
    fn close(&self, id: ResourceId) -> Completion<()> {
        Completion::Ready(
            self.state
                .borrow_mut()
                .open
                .remove(&id)
                .map(|_| ())
                .ok_or_else(HostIoError::invalid),
        )
    }
    fn remove(&self, path: &[u8]) -> Completion<()> {
        Completion::Ready((|| {
            self.mutable()?;
            self.state
                .borrow_mut()
                .files
                .remove(path)
                .map(|_| ())
                .ok_or_else(|| {
                    HostIoError::new(HostIoErrorKind::NotFound, b"file not found".to_vec())
                })
        })())
    }
    fn rename(&self, from: &[u8], to: &[u8]) -> Completion<()> {
        Completion::Ready((|| {
            self.mutable()?;
            let mut s = self.state.borrow_mut();
            let d = Self::named(&s, from)?;
            if from != to {
                s.files.remove(from);
                s.files.insert(to.to_vec(), d);
            }
            Ok(())
        })())
    }
    fn temp_file(&self) -> Completion<ResourceId> {
        Completion::Ready((|| {
            self.mutable()?;
            let mut s = self.state.borrow_mut();
            let id = self.room(&s)?;
            s.next = id.0;
            s.open.insert(
                id,
                OpenFile {
                    data: Rc::new(RefCell::new(Vec::new())),
                    mode: OpenMode::parse(b"w+")?,
                },
            );
            Ok(id)
        })())
    }
    fn temp_name(&self) -> Completion<Vec<u8>> {
        Completion::Ready((|| {
            self.mutable()?;
            let mut s = self.state.borrow_mut();
            loop {
                s.temp = s.temp.checked_add(1).ok_or_else(HostIoError::invalid)?;
                let path = format!(".moonseed-temp-{:016x}", s.temp).into_bytes();
                if !s.files.contains_key(&path) {
                    s.files
                        .insert(path.clone(), Rc::new(RefCell::new(Vec::new())));
                    return Ok(path);
                }
            }
        })())
    }
    fn read_file(&self, path: &[u8], max: usize) -> Completion<Vec<u8>> {
        Completion::Ready((|| {
            self.bounded(max)?;
            let s = self.state.borrow();
            let d = Self::named(&s, path)?;
            let data = d.borrow();
            if data.len() > max {
                return Err(HostIoError::denied());
            }
            Ok(data.clone())
        })())
    }
    fn read_file_range(&self, path: &[u8], offset: u64, max: usize) -> Completion<Vec<u8>> {
        Completion::Ready((|| {
            self.bounded(max)?;
            let s = self.state.borrow();
            let d = Self::named(&s, path)?;
            let data = d.borrow();
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(data.len());
            Ok(data[start..start.saturating_add(max).min(data.len())].to_vec())
        })())
    }
    fn handle_policy(&self) -> HandlePolicy {
        HandlePolicy::Rebind
    }
    fn rebind(&self, id: ResourceId) -> Result<(), HostIoError> {
        self.handle(id).map(|_| ())
    }
}
