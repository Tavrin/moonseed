#![cfg(unix)]
//! Standalone runner status and shutdown, through the installed public OS library.
#[test]
fn host_exit_status_and_close_are_observed_by_the_runner() {
    let root = std::env::temp_dir().join(format!(
        "moonseed-rv-exit-{}-{}",
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
    for (status, expected) in [("nil", 0), ("true", 0), ("false", 1), ("7", 7), ("-1", 255)] {
        for close in [false, true] {
            let source = format!(
                "local keep=setmetatable({{}},{{__gc=function() print('CLOSED') end}}); pcall(function() os.exit({status},{close}) end); print('AFTER')"
            );
            let path = root.join("exit.lua");
            std::fs::write(&path, source).unwrap();
            let output = std::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    "ulimit -v 2000000; exec timeout 60 \"$@\"",
                    "moonseed-exit-test",
                    env!("CARGO_BIN_EXE_moonseed-run"),
                    path.to_str().unwrap(),
                    "--lua-args",
                ])
                .current_dir(&root)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(expected),
                "{status}/{close}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                output.stdout,
                if close { b"CLOSED\n".as_slice() } else { b"" },
                "{status}/{close}"
            );
            assert!(
                output.stderr.is_empty(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn review_vfs_import_preserves_byte_paths_and_rejects_symlinks() {
    use std::os::unix::ffi::OsStringExt;
    let root = std::env::temp_dir().join(format!(
        "moonseed-rv-vfs-{}-{}",
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
    let fixture = root.join("fixture");
    std::fs::create_dir(&fixture).unwrap();
    std::fs::create_dir(fixture.join("empty")).unwrap();
    std::fs::write(
        fixture.join(std::ffi::OsString::from_vec(b"f\xff".to_vec())),
        b"byte-path",
    )
    .unwrap();
    let script = root.join("driver.lua");
    std::fs::write(&script,b"local f <close> = assert(io.open('f'..string.char(255))); assert(f:read('a')=='byte-path'); local o <close> = assert(io.open('empty/new','w')); o:write('ok'); print('PASS')").unwrap();
    let profile = format!("--host=vfs:{}", fixture.display());
    let launch = || {
        std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "ulimit -v 2000000; exec timeout 60 \"$@\"",
                "moonseed-vfs-test",
                env!("CARGO_BIN_EXE_moonseed-run"),
                script.to_str().unwrap(),
                &profile,
            ])
            .current_dir(&root)
            .output()
            .unwrap()
    };
    let output = launch();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"PASS\n");
    std::fs::write(root.join("outside"), b"private sentinel").unwrap();
    std::os::unix::fs::symlink(root.join("outside"), fixture.join("link")).unwrap();
    let output = launch();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("fixture import rejects symlinks"));
}
