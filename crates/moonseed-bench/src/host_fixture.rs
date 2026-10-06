//! The VFS fixture profile models the corpus's existing directory namespace and
//! Linux NotFound text. VM authority still goes only through public traits.
use moonseed::hostcaps::{
    Completion, HandlePolicy, HostIoError, HostIoErrorKind, OpenMode, ResourceId,
};
use moonseed::{Filesystem, MemoryFilesystem};
use std::collections::BTreeSet;
pub struct FixtureFilesystem {
    pub fs: MemoryFilesystem,
    pub directories: BTreeSet<Vec<u8>>,
}
fn missing() -> HostIoError {
    HostIoError::new(
        HostIoErrorKind::NotFound,
        b"No such file or directory".to_vec(),
    )
}
fn result<T>(c: Completion<T>) -> Completion<T> {
    match c {
        Completion::Ready(Err(e)) if e.kind == HostIoErrorKind::NotFound => {
            Completion::Ready(Err(missing()))
        }
        c => c,
    }
}
impl Filesystem for FixtureFilesystem {
    fn open(&self, p: &[u8], m: OpenMode) -> Completion<ResourceId> {
        if p.iter()
            .rposition(|b| *b == b'/')
            .is_some_and(|i| !self.directories.contains(&p[..i]))
        {
            return Completion::Ready(Err(missing()));
        }
        result(self.fs.open(p, m))
    }
    fn read_at(&self, id: ResourceId, o: u64, n: usize) -> Completion<Vec<u8>> {
        self.fs.read_at(id, o, n)
    }
    fn write_at(&self, id: ResourceId, o: u64, b: &[u8]) -> Completion<usize> {
        self.fs.write_at(id, o, b)
    }
    fn append(&self, id: ResourceId, b: &[u8]) -> Completion<u64> {
        self.fs.append(id, b)
    }
    fn size(&self, id: ResourceId) -> Completion<u64> {
        self.fs.size(id)
    }
    fn flush(&self, id: ResourceId) -> Completion<()> {
        self.fs.flush(id)
    }
    fn close(&self, id: ResourceId) -> Completion<()> {
        self.fs.close(id)
    }
    fn temp_file(&self) -> Completion<ResourceId> {
        self.fs.temp_file()
    }
    fn temp_name(&self) -> Completion<Vec<u8>> {
        self.fs.temp_name()
    }
    fn remove(&self, p: &[u8]) -> Completion<()> {
        result(self.fs.remove(p))
    }
    fn rename(&self, p: &[u8], q: &[u8]) -> Completion<()> {
        result(self.fs.rename(p, q))
    }
    fn read_file(&self, p: &[u8], n: usize) -> Completion<Vec<u8>> {
        result(self.fs.read_file(p, n))
    }
    fn probe_readable(&self, p: &[u8]) -> Completion<bool> {
        self.fs.probe_readable(p)
    }
    fn read_file_range(&self, path: &[u8], offset: u64, max: usize) -> Completion<Vec<u8>> {
        result(self.fs.read_file_range(path, offset, max))
    }
    fn handle_policy(&self) -> HandlePolicy {
        HandlePolicy::Rebind
    }
    fn rebind(&self, id: ResourceId) -> Result<(), HostIoError> {
        self.fs.rebind(id)
    }
}
