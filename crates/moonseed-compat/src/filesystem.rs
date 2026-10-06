//! Read-only suite files over a writable, per-file native scratch directory.
use moonseed::hostcaps::Completion;
use moonseed::{Filesystem, HostIoError, HostIoErrorKind, NativeFilesystem, OpenMode, ResourceId};

pub struct SuiteFilesystem {
    pub suite: NativeFilesystem,
    pub scratch: NativeFilesystem,
}
const SUITE_ID: u64 = 1 << 63;
impl SuiteFilesystem {
    fn backend(&self, id: ResourceId) -> (&NativeFilesystem, ResourceId) {
        if id.0 & SUITE_ID != 0 {
            (&self.suite, ResourceId(id.0 & !SUITE_ID))
        } else {
            (&self.scratch, id)
        }
    }
    fn fallback<T>(
        &self,
        answer: Completion<T>,
        read: impl FnOnce() -> Completion<T>,
    ) -> Completion<T> {
        match answer {
            Completion::Ready(Err(e)) if e.kind == HostIoErrorKind::NotFound => read(),
            other => other,
        }
    }
}
impl Filesystem for SuiteFilesystem {
    fn probe_readable(&self, p: &[u8]) -> Completion<bool> {
        match self.suite.probe_readable(p) {
            Completion::Ready(Ok(false)) => self.scratch.probe_readable(p),
            other => other,
        }
    }
    fn open(&self, p: &[u8], m: OpenMode) -> Completion<ResourceId> {
        // Existing suite files cannot be shadowed, truncated, or appended to.
        if matches!(self.suite.probe_readable(p), Completion::Ready(Ok(true))) {
            return match self.suite.open(p, m) {
                Completion::Ready(Ok(id)) => Completion::Ready(Ok(ResourceId(id.0 | SUITE_ID))),
                other => other,
            };
        }
        self.scratch.open(p, m)
    }
    fn read_at(&self, id: ResourceId, o: u64, n: usize) -> Completion<Vec<u8>> {
        let (fs, id) = self.backend(id);
        fs.read_at(id, o, n)
    }
    fn write_at(&self, id: ResourceId, o: u64, b: &[u8]) -> Completion<usize> {
        let (fs, id) = self.backend(id);
        fs.write_at(id, o, b)
    }
    fn append(&self, id: ResourceId, b: &[u8]) -> Completion<u64> {
        let (fs, id) = self.backend(id);
        fs.append(id, b)
    }
    fn size(&self, id: ResourceId) -> Completion<u64> {
        let (fs, id) = self.backend(id);
        fs.size(id)
    }
    fn flush(&self, id: ResourceId) -> Completion<()> {
        let (fs, id) = self.backend(id);
        fs.flush(id)
    }
    fn close(&self, id: ResourceId) -> Completion<()> {
        let (fs, id) = self.backend(id);
        fs.close(id)
    }
    fn remove(&self, p: &[u8]) -> Completion<()> {
        self.scratch.remove(p)
    }
    fn rename(&self, a: &[u8], b: &[u8]) -> Completion<()> {
        if matches!(self.suite.probe_readable(b), Completion::Ready(Ok(true))) {
            return Completion::Ready(Err(HostIoError::new(
                HostIoErrorKind::PermissionDenied,
                b"suite files are read-only".to_vec(),
            )));
        }
        self.scratch.rename(a, b)
    }
    fn temp_file(&self) -> Completion<ResourceId> {
        self.scratch.temp_file()
    }
    fn temp_name(&self) -> Completion<Vec<u8>> {
        self.scratch.temp_name()
    }
    fn read_file(&self, p: &[u8], n: usize) -> Completion<Vec<u8>> {
        self.fallback(self.suite.read_file(p, n), || self.scratch.read_file(p, n))
    }
    fn read_file_range(&self, p: &[u8], o: u64, n: usize) -> Completion<Vec<u8>> {
        self.fallback(self.suite.read_file_range(p, o, n), || {
            self.scratch.read_file_range(p, o, n)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ready<T>(result: Completion<T>) -> T {
        match result {
            Completion::Ready(Ok(value)) => value,
            _ => panic!("filesystem failed"),
        }
    }
    #[test]
    fn review_suite_open_and_load_share_namespace_precedence() {
        let root = std::env::temp_dir().join(format!(
            "moonseed-rv-suite-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).unwrap();
            }
        }
        let _cleanup = Cleanup(root.clone());
        let suite = root.join("suite");
        let scratch = root.join("scratch");
        std::fs::create_dir(&suite).unwrap();
        std::fs::create_dir(&scratch).unwrap();
        std::fs::write(suite.join("module.lua"), b"suite").unwrap();
        std::fs::write(scratch.join("module.lua"), b"shadow").unwrap();
        std::fs::write(scratch.join("generated.lua"), b"generated").unwrap();
        let fs = SuiteFilesystem {
            suite: NativeFilesystem::new(
                suite,
                moonseed::NativeOptions {
                    read_only: true,
                    ..Default::default()
                },
            )
            .unwrap(),
            scratch: NativeFilesystem::new(scratch, Default::default()).unwrap(),
        };
        let id = ready(fs.open(b"module.lua", OpenMode::parse(b"r").unwrap()));
        assert_eq!(ready(fs.read_at(id, 0, 64)), b"suite");
        assert_eq!(ready(fs.read_file(b"module.lua", 64)), b"suite");
        assert_eq!(ready(fs.read_file_range(b"module.lua", 1, 64)), b"uite");
        assert!(ready(fs.probe_readable(b"module.lua")));
        assert_eq!(ready(fs.read_file(b"generated.lua", 64)), b"generated");
        assert!(matches!(
            fs.open(b"module.lua", OpenMode::parse(b"w").unwrap()),
            Completion::Ready(Err(_))
        ));
        ready(fs.close(id));
    }
}
