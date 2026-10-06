//! Explicit frozen-fixture namespace: PUC-shaped /tmp/lua_ names alias secure
//! native reservations inside the supplied root. Other absolute paths retain
//! the native adapter's rejection policy. No additional filesystem authority.
use moonseed::{
    CapabilityCompletion as Completion, Filesystem, HandlePolicy, HostIoError, NativeFilesystem,
    OpenMode, ResourceId,
};

pub struct FixtureTemporaryNames(pub NativeFilesystem);
fn physical(path: &[u8]) -> Vec<u8> {
    if let Some(nonce) = path.strip_prefix(b"/tmp/lua_")
        && nonce.len() == 32
        && nonce.iter().all(u8::is_ascii_hexdigit)
    {
        let mut name = b".moonseed-".to_vec();
        name.extend(nonce);
        name
    } else {
        path.to_vec()
    }
}
impl Filesystem for FixtureTemporaryNames {
    fn temp_name(&self) -> Completion<Vec<u8>> {
        match self.0.temp_name() {
            Completion::Ready(result) => Completion::Ready(result.map(|path| {
                let mut name = b"/tmp/lua_".to_vec();
                name.extend(path.strip_prefix(b".moonseed-").unwrap_or(&path));
                name
            })),
            Completion::Pending(token) => Completion::Pending(token),
        }
    }
    fn probe_readable(&self, path: &[u8]) -> Completion<bool> {
        self.0.probe_readable(&physical(path))
    }
    fn open(&self, path: &[u8], mode: OpenMode) -> Completion<ResourceId> {
        self.0.open(&physical(path), mode)
    }
    fn read_at(&self, id: ResourceId, offset: u64, max: usize) -> Completion<Vec<u8>> {
        self.0.read_at(id, offset, max)
    }
    fn write_at(&self, id: ResourceId, offset: u64, bytes: &[u8]) -> Completion<usize> {
        self.0.write_at(id, offset, bytes)
    }
    fn append(&self, id: ResourceId, bytes: &[u8]) -> Completion<u64> {
        self.0.append(id, bytes)
    }
    fn size(&self, id: ResourceId) -> Completion<u64> {
        self.0.size(id)
    }
    fn flush(&self, id: ResourceId) -> Completion<()> {
        self.0.flush(id)
    }
    fn close(&self, id: ResourceId) -> Completion<()> {
        self.0.close(id)
    }
    fn remove(&self, path: &[u8]) -> Completion<()> {
        self.0.remove(&physical(path))
    }
    fn rename(&self, from: &[u8], to: &[u8]) -> Completion<()> {
        self.0.rename(&physical(from), &physical(to))
    }
    fn temp_file(&self) -> Completion<ResourceId> {
        self.0.temp_file()
    }
    fn read_file(&self, path: &[u8], max: usize) -> Completion<Vec<u8>> {
        self.0.read_file(&physical(path), max)
    }
    fn read_file_range(&self, path: &[u8], offset: u64, max: usize) -> Completion<Vec<u8>> {
        self.0.read_file_range(&physical(path), offset, max)
    }
    fn handle_policy(&self) -> HandlePolicy {
        self.0.handle_policy()
    }
    fn rebind(&self, id: ResourceId) -> Result<(), HostIoError> {
        self.0.rebind(id)
    }
}
