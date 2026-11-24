#![allow(dead_code)]

use std::path::{Path, PathBuf};
use tempfile::TempDir;

pub struct TestProject {
    _project_dir: TempDir,
    _session_dir: TempDir,
    pub root: PathBuf,
    pub session_dir: PathBuf,
}

impl TestProject {
    pub fn new() -> Self {
        let project_dir = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        Self {
            root: project_dir.path().canonicalize().unwrap(),
            session_dir: session_dir.path().to_path_buf(),
            _project_dir: project_dir,
            _session_dir: session_dir,
        }
    }

    pub fn store(&self) -> zed_review_mcp::ReviewStore {
        zed_review_mcp::ReviewStore::start(&self.root, &self.session_dir).unwrap()
    }

    pub fn write_relative(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
        path
    }
}

pub struct FakeZed {
    _script_dir: TempDir,
    pub script_path: PathBuf,
    pub invocation_log: PathBuf,
}

impl FakeZed {
    pub fn new() -> Self {
        let script_dir = TempDir::new().unwrap();
        let script_path = script_dir.path().join("zed");
        let invocation_log = script_dir.path().join("zed-invocations.log");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\n",
            invocation_log.to_string_lossy()
        );
        std::fs::write(&script_path, script).unwrap();
        make_executable(&script_path);
        Self {
            _script_dir: script_dir,
            script_path,
            invocation_log,
        }
    }

    pub fn opener(&self) -> zed_review_mcp::zed_opener::ZedOpener {
        zed_review_mcp::zed_opener::ZedOpener::from_path(self.script_path.clone())
    }

    pub fn recorded_invocations(&self) -> Vec<String> {
        self.wait_for_invocation_count(1);
        std::fs::read_to_string(&self.invocation_log)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.is_empty())
            .map(ToString::to_string)
            .collect()
    }

    pub fn wait_for_invocation(&self) {
        wait_for_invocation_log(&self.invocation_log);
    }

    pub fn wait_for_invocation_count(&self, count: usize) {
        for _ in 0..200 {
            match std::fs::read_to_string(&self.invocation_log) {
                Ok(recorded)
                    if recorded.lines().filter(|line| !line.is_empty()).count() >= count =>
                {
                    return;
                }
                _ => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        panic!("the fake zed executable recorded too few invocations");
    }
}

pub fn wait_for_invocation_log(log_path: &Path) {
    for _ in 0..200 {
        if log_path.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("the fake zed executable was never invoked");
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {
    panic!("the fake zed helper requires unix");
}
