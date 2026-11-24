use anyhow::{Context as _, Result, anyhow};
use smol::process::{Command, Stdio};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct ZedOpener {
    zed_executable_path: PathBuf,
}

impl ZedOpener {
    pub fn resolve() -> Result<Self> {
        Self::resolve_from(std::env::var_os("ZED_PATH"), std::env::var_os("PATH"))
    }

    fn resolve_from(zed_path_env: Option<OsString>, path_env: Option<OsString>) -> Result<Self> {
        let zed_executable_path = match zed_path_env {
            Some(configured_path) => {
                let configured_path = PathBuf::from(configured_path);
                anyhow::ensure!(
                    configured_path.is_file(),
                    "ZED_PATH points to {}, which is not a file; \
                     unset it to use `zed` from PATH",
                    configured_path.display()
                );
                configured_path
            }
            None => find_in_path("zed", path_env).ok_or_else(|| {
                anyhow!(
                    "cannot find the `zed` executable; install the Zed CLI or point the \
                     ZED_PATH environment variable at the zed binary"
                )
            })?,
        };
        Ok(Self {
            zed_executable_path,
        })
    }

    pub fn from_path(zed_executable_path: PathBuf) -> Self {
        Self {
            zed_executable_path,
        }
    }

    pub async fn open_diff(&self, left: &Path, right: &Path) -> Result<()> {
        anyhow::ensure!(
            left.is_dir(),
            "diff left side {} is not a directory",
            left.display()
        );
        anyhow::ensure!(
            right.is_dir(),
            "diff right side {} is not a directory",
            right.display()
        );
        let arguments = vec![
            "--diff".to_string(),
            left.to_string_lossy().to_string(),
            right.to_string_lossy().to_string(),
        ];
        let action_description = format!(
            "opening the diff between {} and {} in Zed",
            left.display(),
            right.display()
        );
        self.spawn_detached(&arguments, &action_description).await
    }

    pub async fn open_paths(&self, paths: &[PathBuf]) -> Result<()> {
        anyhow::ensure!(
            !paths.is_empty(),
            "opening files in Zed requires at least one path"
        );
        let mut arguments = Vec::new();
        for path in paths {
            anyhow::ensure!(
                path.exists(),
                "cannot open {} in Zed: the path does not exist",
                path.display()
            );
            arguments.push(path.to_string_lossy().to_string());
        }
        self.spawn_detached(&arguments, "opening paths in Zed")
            .await
    }

    async fn spawn_detached(&self, arguments: &[String], action_description: &str) -> Result<()> {
        let mut command = Command::new(&self.zed_executable_path);
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .reap_on_drop(true);
        command
            .spawn()
            .with_context(|| {
                format!(
                    "{action_description}: spawning {}",
                    self.zed_executable_path.display()
                )
            })
            .map(|_| ())
    }
}

fn find_in_path(program_name: &str, path_env: Option<OsString>) -> Option<PathBuf> {
    let path_variable = path_env?;
    for directory in std::env::split_paths(&path_variable) {
        let candidate = directory.join(program_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn fake_zed_script(directory: &Path) -> (PathBuf, PathBuf) {
        let script_path = directory.join("zed");
        let record_path = directory.join("zed-invocations.log");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\n",
            record_path.to_string_lossy()
        );
        fs::write(&script_path, script).unwrap();
        make_executable(&script_path);
        (script_path, record_path)
    }

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(not(unix))]
    fn make_executable(_path: &Path) {
        panic!("fake zed helper requires unix");
    }

    fn recorded_invocations(record_path: &Path) -> Vec<String> {
        fs::read_to_string(record_path)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.is_empty())
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn test_find_in_path_uses_path_environment() {
        let temp_dir = TempDir::new().unwrap();
        let (script_path, _record_path) = fake_zed_script(temp_dir.path());
        let found = find_in_path("zed", Some(OsString::from(temp_dir.path())));
        assert_eq!(
            found.map(|path| path.canonicalize().unwrap()),
            Some(script_path.canonicalize().unwrap())
        );
        assert!(find_in_path("zed", Some(OsString::from(""))).is_none());
    }

    #[test]
    fn test_resolve_prefers_zed_path_over_path_lookup() {
        let temp_dir = TempDir::new().unwrap();
        let (script_path, _record_path) = fake_zed_script(temp_dir.path());
        let opener = ZedOpener::resolve_from(Some(OsString::from(&script_path)), None).unwrap();
        assert_eq!(opener.zed_executable_path, script_path);
    }

    #[test]
    fn test_resolve_rejects_invalid_zed_path() {
        let temp_dir = TempDir::new().unwrap();
        let error =
            ZedOpener::resolve_from(Some(OsString::from(temp_dir.path().join("missing"))), None)
                .unwrap_err()
                .to_string();
        assert!(error.contains("ZED_PATH"), "{error}");
    }

    #[test]
    fn test_open_diff_spawns_zed_with_directory_pair() {
        let temp_dir = TempDir::new().unwrap();
        let (script_path, record_path) = fake_zed_script(temp_dir.path());
        let opener = ZedOpener::from_path(script_path);
        let left = temp_dir.path().join("left");
        let right = temp_dir.path().join("right");
        fs::create_dir_all(&left).unwrap();
        fs::create_dir_all(&right).unwrap();

        smol::block_on(opener.open_diff(&left, &right)).unwrap();
        wait_for_invocation(&record_path);

        let invocations = recorded_invocations(&record_path);
        assert_eq!(
            invocations,
            vec![format!("--diff {} {}", left.display(), right.display())]
        );
    }

    #[test]
    fn test_open_diff_rejects_missing_directories() {
        let temp_dir = TempDir::new().unwrap();
        let (script_path, _record_path) = fake_zed_script(temp_dir.path());
        let opener = ZedOpener::from_path(script_path);
        let missing = temp_dir.path().join("nope");
        let error = smol::block_on(opener.open_diff(&missing, temp_dir.path()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("not a directory"), "{error}");
    }

    #[test]
    fn test_open_paths_spawns_zed_with_paths() {
        let temp_dir = TempDir::new().unwrap();
        let (script_path, record_path) = fake_zed_script(temp_dir.path());
        let opener = ZedOpener::from_path(script_path);
        let first_file = temp_dir.path().join("one.py");
        let second_file = temp_dir.path().join("two.py");
        fs::write(&first_file, "1\n").unwrap();
        fs::write(&second_file, "2\n").unwrap();

        smol::block_on(opener.open_paths(&[first_file.clone(), second_file.clone()])).unwrap();
        wait_for_invocation(&record_path);

        let invocations = recorded_invocations(&record_path);
        assert_eq!(
            invocations,
            vec![format!(
                "{} {}",
                first_file.display(),
                second_file.display()
            )]
        );
    }

    fn wait_for_invocation(record_path: &Path) {
        for _ in 0..100 {
            if record_path.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the fake zed executable was never invoked");
    }
}
