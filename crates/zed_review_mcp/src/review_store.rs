use crate::trusted_root::confine;
use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Created,
    Modified,
    Removed,
    Unchanged,
}

impl std::fmt::Display for FileKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            FileKind::Created => "created",
            FileKind::Modified => "modified",
            FileKind::Removed => "removed",
            FileKind::Unchanged => "unchanged",
        };
        formatter.write_str(name)
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStatus {
    pub path: PathBuf,
    pub kind: FileKind,
}

impl FileStatus {
    pub fn relative_path_string(&self) -> String {
        self.path.to_string_lossy().to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    Recorded,
    MarkedForCreation,
}

enum BaselineState {
    CreationMarker,
    ContentWithLength(u64),
}

#[derive(Debug)]
pub struct ReviewStore {
    trusted_root: PathBuf,
    session_dir: PathBuf,
    baselines_dir: PathBuf,
    workspace_lock: Mutex<()>,
}

const TRUSTED_ROOT_FILE: &str = "trusted-root";
pub const BASELINES_DIR: &str = "baselines";

impl ReviewStore {
    pub fn start(trusted_root: &Path, session_dir: &Path) -> Result<Self> {
        let trusted_root = trusted_root
            .canonicalize()
            .with_context(|| format!("resolving trusted root {}", trusted_root.display()))?;
        fs::create_dir_all(session_dir)
            .with_context(|| format!("creating session directory {}", session_dir.display()))?;
        let session_dir = session_dir
            .canonicalize()
            .with_context(|| format!("resolving session directory {}", session_dir.display()))?;
        let baselines_dir = session_dir.join(BASELINES_DIR);

        let trusted_root_file = session_dir.join(TRUSTED_ROOT_FILE);
        if trusted_root_file.exists() {
            let recorded_root = fs::read_to_string(&trusted_root_file)
                .with_context(|| format!("reading {}", trusted_root_file.display()))?;
            anyhow::ensure!(
                recorded_root.trim() == trusted_root.to_string_lossy(),
                "session directory {} was started for trusted root {recorded_root}, not {}",
                session_dir.display(),
                trusted_root.display()
            );
            anyhow::ensure!(
                baselines_dir.exists(),
                "session directory {} is missing its {} directory",
                session_dir.display(),
                BASELINES_DIR
            );
            anyhow::ensure!(
                fs::read_dir(&baselines_dir)
                    .with_context(|| format!("opening {}", baselines_dir.display()))?
                    .next()
                    .is_none(),
                "session directory {} already contains baselines for another review; \
                 start a fresh session for a new review round",
                session_dir.display()
            );
        } else {
            fs::create_dir_all(&baselines_dir)
                .with_context(|| format!("creating {}", baselines_dir.display()))?;
            fs::write(
                &trusted_root_file,
                trusted_root.to_string_lossy().as_bytes(),
            )
            .with_context(|| format!("writing {}", trusted_root_file.display()))?;
        }

        Ok(Self {
            trusted_root,
            session_dir,
            baselines_dir,
            workspace_lock: Mutex::new(()),
        })
    }

    pub fn trusted_root(&self) -> &Path {
        &self.trusted_root
    }

    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    pub fn baselines_dir(&self) -> &Path {
        &self.baselines_dir
    }

    fn relative_to_trusted_root(&self, path: &Path) -> Result<PathBuf> {
        path.strip_prefix(&self.trusted_root)
            .map(PathBuf::from)
            .with_context(|| format!("{} is not under the trusted root", path.display()))
    }

    fn baseline_path(&self, path: &Path) -> Result<PathBuf> {
        let confined = confine(path, &self.trusted_root)?;
        let relative = self.relative_to_trusted_root(&confined)?;
        Ok(self.baselines_dir.join(relative))
    }

    fn baseline_state(baseline_path: &Path) -> Result<BaselineState> {
        let metadata = fs::metadata(baseline_path)
            .with_context(|| format!("reading baseline snapshot {}", baseline_path.display()))?;
        anyhow::ensure!(
            metadata.is_file(),
            "baseline snapshot {} is not a file",
            baseline_path.display()
        );
        let length = metadata.len();
        Ok(if length == 0 {
            BaselineState::CreationMarker
        } else {
            BaselineState::ContentWithLength(length)
        })
    }

    fn read_baseline(baseline_path: &Path) -> Result<Vec<u8>> {
        let contents = fs::read(baseline_path)
            .with_context(|| format!("reading baseline snapshot {}", baseline_path.display()))?;
        Ok(contents)
    }

    fn remove_baseline(&self, baseline_path: &Path) -> Result<()> {
        fs::remove_file(baseline_path)
            .with_context(|| format!("removing baseline snapshot {}", baseline_path.display()))?;
        self.prune_baseline_parent_dirs(baseline_path);
        Ok(())
    }

    fn prune_baseline_parent_dirs(&self, baseline_path: &Path) {
        let mut directory = baseline_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        while directory.starts_with(&self.baselines_dir) && directory != self.baselines_dir {
            if fs::remove_dir(&directory).is_err() {
                return;
            }
            directory.pop();
        }
    }

    pub fn record_baseline(&self, path: &Path) -> Result<RecordOutcome> {
        let _guard = self
            .workspace_lock
            .lock()
            .map_err(|_| anyhow!("review store workspace lock poisoned"))?;
        let baseline_path = self.baseline_path(path)?;
        anyhow::ensure!(
            !baseline_path.exists(),
            "{} is already under review (it has a baseline from this round); \
             approve or reject it before recording a new baseline",
            path.display()
        );
        let outcome = match path.canonicalize() {
            Ok(existing) => {
                let metadata = fs::metadata(&existing)
                    .with_context(|| format!("reading {}", existing.display()))?;
                anyhow::ensure!(
                    metadata.is_file(),
                    "{} is a directory; only files can be reviewed",
                    path.display()
                );
                let contents = fs::read(&existing)
                    .with_context(|| format!("reading {}", existing.display()))?;
                let contents = String::from_utf8(contents)
                    .map_err(|_| {
                        anyhow!(
                            "{} is not a UTF-8 text file; only text files can be reviewed",
                            path.display()
                        )
                    })?
                    .into_bytes();
                if let Some(parent) = baseline_path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("creating {}", parent.display()))?;
                }
                fs::write(&baseline_path, contents).with_context(|| {
                    format!("writing baseline snapshot {}", baseline_path.display())
                })?;
                RecordOutcome::Recorded
            }
            Err(_) => {
                if let Some(parent) = baseline_path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("creating {}", parent.display()))?;
                }
                fs::write(&baseline_path, b"").with_context(|| {
                    format!("writing baseline snapshot {}", baseline_path.display())
                })?;
                RecordOutcome::MarkedForCreation
            }
        };
        Ok(outcome)
    }

    pub fn has_baseline(&self, path: &Path) -> Result<bool> {
        Ok(self.baseline_path(path)?.exists())
    }

    pub fn materialize_review_view(&self) -> Result<(PathBuf, Vec<FileStatus>)> {
        let review_view_dir = self.session_dir.join("review-view");
        if review_view_dir.exists() {
            fs::remove_dir_all(&review_view_dir)
                .with_context(|| format!("clearing {}", review_view_dir.display()))?;
        }
        fs::create_dir_all(&review_view_dir)
            .with_context(|| format!("creating {}", review_view_dir.display()))?;

        let statuses = self.status()?;
        let mut changed_statuses = Vec::new();
        for file_status in &statuses {
            if file_status.kind == FileKind::Unchanged {
                continue;
            }
            let baseline_path = self.baselines_dir.join(&file_status.path);
            let view_path = review_view_dir.join(&file_status.path);
            if let Some(parent) = view_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            fs::copy(&baseline_path, &view_path).with_context(|| {
                format!(
                    "copying {} to {}",
                    baseline_path.display(),
                    view_path.display()
                )
            })?;
            changed_statuses.push(FileStatus {
                path: file_status.path.clone(),
                kind: file_status.kind,
            });
        }
        Ok((review_view_dir, changed_statuses))
    }

    fn reviewed_baseline_paths(&self) -> Result<Vec<(PathBuf, PathBuf)>> {
        let mut reviewed = Vec::new();
        let mut stack = vec![self.baselines_dir.clone()];
        while let Some(directory) = stack.pop() {
            let entries = fs::read_dir(&directory)
                .with_context(|| format!("reading {}", directory.display()))?;
            for entry in entries {
                let entry = entry.with_context(|| format!("reading {}", directory.display()))?;
                let entry_path = entry.path();
                let file_type = entry
                    .file_type()
                    .with_context(|| format!("reading {}", entry_path.display()))?;
                if file_type.is_dir() {
                    stack.push(entry_path);
                } else if file_type.is_file() {
                    let relative = entry_path
                        .strip_prefix(&self.baselines_dir)
                        .context("baseline snapshot outside baselines dir")
                        .map(PathBuf::from)?;
                    reviewed.push((relative, entry_path));
                }
            }
        }
        reviewed.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(reviewed)
    }

    pub fn status(&self) -> Result<Vec<FileStatus>> {
        let _guard = self
            .workspace_lock
            .lock()
            .map_err(|_| anyhow!("review store workspace lock poisoned"))?;
        let mut statuses = Vec::new();
        for (relative, baseline_path) in self.reviewed_baseline_paths()? {
            let working_path = self.trusted_root.join(&relative);
            let kind = match Self::baseline_state(&baseline_path)? {
                BaselineState::CreationMarker => {
                    if working_path.exists() && !working_path.is_file() {
                        anyhow::bail!(
                            "{} exists but is not a file; \
                             the creation baseline cannot be compared",
                            working_path.display()
                        );
                    }
                    FileKind::Created
                }
                BaselineState::ContentWithLength(_length) => {
                    if !working_path.exists() {
                        FileKind::Removed
                    } else if Self::read_baseline(&baseline_path)? == fs::read(&working_path)? {
                        FileKind::Unchanged
                    } else {
                        FileKind::Modified
                    }
                }
            };
            statuses.push(FileStatus {
                path: relative,
                kind,
            });
        }
        Ok(statuses)
    }

    fn reviewed_paths(&self) -> Result<Vec<PathBuf>> {
        Ok(self
            .reviewed_baseline_paths()?
            .into_iter()
            .map(|(relative, _baseline_path)| self.trusted_root.join(relative))
            .collect())
    }

    pub fn approve_all(&self) -> Result<usize> {
        let _guard = self
            .workspace_lock
            .lock()
            .map_err(|_| anyhow!("review store workspace lock poisoned"))?;
        let baselines = self
            .reviewed_baseline_paths()?
            .into_iter()
            .map(|(_relative, baseline_path)| baseline_path)
            .collect();
        self.remove_baselines(baselines)
    }

    pub fn approve_paths(&self, paths: &[PathBuf]) -> Result<usize> {
        anyhow::ensure!(
            !paths.is_empty(),
            "approve requires at least one path (use approve_all to approve everything)"
        );
        let _guard = self
            .workspace_lock
            .lock()
            .map_err(|_| anyhow!("review store workspace lock poisoned"))?;
        let mut baselines = Vec::new();
        for path in paths {
            let baseline_path = self.baseline_path(path)?;
            anyhow::ensure!(
                baseline_path.exists(),
                "{} has no baseline; it is not under review",
                path.display()
            );
            baselines.push(baseline_path);
        }
        self.remove_baselines(baselines)
    }

    pub fn reject_all(&self) -> Result<usize> {
        let _guard = self
            .workspace_lock
            .lock()
            .map_err(|_| anyhow!("review store workspace lock poisoned"))?;
        let targets = self.reviewed_paths()?;
        self.reject_targets(targets)
    }

    pub fn reject_paths(&self, paths: &[PathBuf]) -> Result<usize> {
        anyhow::ensure!(
            !paths.is_empty(),
            "reject requires at least one path (use reject_all to reject everything)"
        );
        let _guard = self
            .workspace_lock
            .lock()
            .map_err(|_| anyhow!("review store workspace lock poisoned"))?;
        self.reject_targets(paths.to_vec())
    }

    fn reject_targets(&self, targets: Vec<PathBuf>) -> Result<usize> {
        let mut restored_count = 0;
        for target in targets {
            let baseline_path = self.baseline_path(&target)?;
            anyhow::ensure!(
                baseline_path.exists(),
                "{} has no baseline; it is not under review",
                target.display()
            );
            match Self::baseline_state(&baseline_path)? {
                BaselineState::CreationMarker => {
                    if target.exists() {
                        if target.is_dir() {
                            anyhow::bail!(
                                "{} is a directory; rejecting a created file cannot remove a directory",
                                target.display()
                            );
                        }
                        fs::remove_file(&target).with_context(|| {
                            format!("removing created file {}", target.display())
                        })?;
                    }
                }
                BaselineState::ContentWithLength(_) => {
                    let baseline_contents = Self::read_baseline(&baseline_path)?;
                    let contents = String::from_utf8(baseline_contents).map_err(|_| {
                        anyhow!(
                            "baseline snapshot {} is not valid UTF-8",
                            baseline_path.display()
                        )
                    })?;
                    write_atomically(&target, &contents)?;
                }
            }
            self.remove_baseline(&baseline_path)?;
            restored_count += 1;
        }
        Ok(restored_count)
    }

    fn remove_baselines(&self, baseline_targets: Vec<PathBuf>) -> Result<usize> {
        let mut removed_count = 0;
        for baseline_path in baseline_targets {
            anyhow::ensure!(
                baseline_path.exists(),
                "baseline snapshot {} is missing; it is not under review",
                baseline_path.display()
            );
            self.remove_baseline(&baseline_path)?;
            removed_count += 1;
        }
        Ok(removed_count)
    }
}

pub fn write_atomically(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let mut temporary_path = parent.join(format!(
        ".{}.zed-review-mcp.tmp",
        path.file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default()
    ));
    let mut attempt = 0;
    while temporary_path.exists() {
        attempt += 1;
        temporary_path.set_file_name(format!(
            ".{}.zed-review-mcp.tmp.{attempt}",
            path.file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default()
        ));
    }
    fs::write(&temporary_path, contents)
        .with_context(|| format!("writing {}", temporary_path.display()))?;
    fs::rename(&temporary_path, path)
        .with_context(|| format!("moving {} to {}", temporary_path.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct ReviewFixture {
        _project_dir: TempDir,
        _session_dir: TempDir,
        root: PathBuf,
        store: ReviewStore,
    }

    impl ReviewFixture {
        fn new() -> Self {
            let project_dir = TempDir::new().unwrap();
            let session_dir = TempDir::new().unwrap();
            let root = project_dir.path().canonicalize().unwrap();
            let store = ReviewStore::start(&root, session_dir.path()).unwrap();
            Self {
                _project_dir: project_dir,
                _session_dir: session_dir,
                root,
                store,
            }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.root.join(relative)
        }

        fn write(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.path(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            path
        }
    }

    fn file_status_kinds(statuses: &[FileStatus]) -> Vec<(String, FileKind)> {
        statuses
            .iter()
            .map(|status| (status.relative_path_string(), status.kind))
            .collect()
    }

    #[test]
    fn test_hover_fixture() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("src/main.py", "print('hi')\n");
        assert_eq!(
            fixture.store.record_baseline(&path).unwrap(),
            RecordOutcome::Recorded
        );
        assert_eq!(
            file_status_kinds(&fixture.store.status().unwrap()),
            vec![("src/main.py".into(), FileKind::Unchanged)]
        );
    }

    #[test]
    fn test_record_and_kinds_transitions() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("diary poetry.txt", "baa\n");
        assert_eq!(
            fixture.store.record_baseline(&path).unwrap(),
            RecordOutcome::Recorded
        );
        assert_eq!(
            file_status_kinds(&fixture.store.status().unwrap()),
            vec![("diary poetry.txt".into(), FileKind::Unchanged)]
        );

        fixture.write("diary poetry.txt", "bía\n");
        assert_eq!(
            file_status_kinds(&fixture.store.status().unwrap()),
            vec![("diary poetry.txt".into(), FileKind::Modified)]
        );

        fs::remove_file(&path).unwrap();
        assert_eq!(
            file_status_kinds(&fixture.store.status().unwrap()),
            vec![("diary poetry.txt".into(), FileKind::Removed)]
        );
    }

    #[test]
    fn test_marked_for_creation_round_trip() {
        let fixture = ReviewFixture::new();
        let path = fixture.path("whole_new_file.py");
        assert_eq!(
            fixture.store.record_baseline(&path).unwrap(),
            RecordOutcome::MarkedForCreation
        );
        assert_eq!(
            file_status_kinds(&fixture.store.status().unwrap()),
            vec![("whole_new_file.py".into(), FileKind::Created)]
        );

        fixture.write("whole_new_file.py", "x = 1\n");
        assert_eq!(
            file_status_kinds(&fixture.store.status().unwrap()),
            vec![("whole_new_file.py".into(), FileKind::Created)]
        );

        fixture
            .store
            .approve_paths(std::slice::from_ref(&path))
            .unwrap();
        assert!(fixture.store.status().unwrap().is_empty());
        assert!(path.is_file());
    }

    #[test]
    fn test_marked_for_creation_stays_created_before_file_exists() {
        let fixture = ReviewFixture::new();
        let path = fixture.path("ghost.py");
        fixture.store.record_baseline(&path).unwrap();
        assert_eq!(
            file_status_kinds(&fixture.store.status().unwrap()),
            vec![("ghost.py".into(), FileKind::Created)]
        );
        fixture.write("ghost.py", "");
        fs::remove_file(&path).unwrap();
        assert_eq!(
            file_status_kinds(&fixture.store.status().unwrap()),
            vec![("ghost.py".into(), FileKind::Created)]
        );
    }

    #[test]
    fn test_re_record_rejected_with_offending_path() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("main.py", "print('hi')\n");
        fixture.store.record_baseline(&path).unwrap();
        let error_message = fixture
            .store
            .record_baseline(&path)
            .unwrap_err()
            .to_string();
        assert!(error_message.contains("main.py"), "{error_message}");
        assert!(
            error_message.contains("already under review"),
            "{error_message}"
        );
    }

    #[test]
    fn test_record_rejects_directory() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("nested/inner.txt", "x");
        let messages = fixture
            .store
            .record_baseline(&path.parent().unwrap())
            .unwrap_err();
        assert!(messages.to_string().contains("nested"), "{}", messages);
    }

    #[test]
    fn test_record_rejects_binary_file() {
        let fixture = ReviewFixture::new();
        let path = fixture.path("binary.bin");
        fs::write(&path, [0u8, 159, 146, 150]).unwrap();
        let error = fixture
            .store
            .record_baseline(&path)
            .unwrap_err()
            .to_string();
        assert!(error.contains("binary.bin"), "{error}");
        assert!(error.contains("UTF-8"), "{error}");
    }

    #[test]
    fn test_record_rejects_paths_outside_trusted_root() {
        let fixture = ReviewFixture::new();
        let other_project = TempDir::new().unwrap();
        let outside = other_project.path().join("symlink_target.txt");
        fs::write(&outside, "x").unwrap();
        let error = fixture
            .store
            .record_baseline(&outside)
            .unwrap_err()
            .to_string();
        assert!(error.contains("outside the review trusted root"), "{error}");

        let confined_absolute = outside.canonicalize().unwrap();
        let error = fixture
            .store
            .record_baseline(&confined_absolute)
            .unwrap_err()
            .to_string();
        assert!(error.contains("outside the review trusted root"), "{error}");
    }

    #[test]
    fn test_record_follows_symlink_into_root() {
        let fixture = ReviewFixture::new();
        fixture.write("real_main.py", "y = 2\n");
        let link = fixture.root.join("src/../link.py");
        fs::create_dir_all(fixture.root.join("src")).unwrap();
        std::os::unix::fs::symlink(fixture.root.join("real_main.py"), &link).unwrap();
        assert_eq!(
            fixture.store.record_baseline(&link).unwrap(),
            RecordOutcome::Recorded
        );
        assert!(fixture.store.baselines_dir.join("real_main.py").is_file());
    }

    #[test]
    fn test_reject_restores_content_and_ends_review() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("src/main.py", "print('original')\n");
        fixture.store.record_baseline(&path).unwrap();
        fixture.write("src/main.py", "print('proposed')\n");

        fixture
            .store
            .reject_paths(std::slice::from_ref(&path))
            .unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "print('original')\n");
        assert!(fixture.store.status().unwrap().is_empty());
    }

    #[test]
    fn test_reject_restores_missing_parent_directory_file() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("a/b/file.txt", "one\n");
        fixture.store.record_baseline(&path).unwrap();
        fs::remove_dir_all(fixture.root.join("a")).unwrap();

        fixture
            .store
            .reject_paths(std::slice::from_ref(&path))
            .unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "one\n");
        assert!(fixture.store.status().unwrap().is_empty());
    }

    #[test]
    fn test_reject_errors_when_no_baseline() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("wrong.py", "x");
        let error = fixture
            .store
            .reject_paths(std::slice::from_ref(&path))
            .unwrap_err()
            .to_string();
        assert!(error.contains("wrong.py"), "{error}");
        assert!(error.contains("no baseline"), "{error}");
    }

    #[test]
    fn test_approve_keeps_working_tree_and_ends_review() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("main.py", "a\n");
        fixture.store.record_baseline(&path).unwrap();
        fixture.write("main.py", "b\n");

        fixture
            .store
            .approve_paths(std::slice::from_ref(&path))
            .unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "b\n");
        assert!(fixture.store.status().unwrap().is_empty());
    }

    #[test]
    fn test_approve_errors_when_no_baseline() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("unreviewed.py", "x");
        let error = fixture
            .store
            .approve_paths(std::slice::from_ref(&path))
            .unwrap_err()
            .to_string();
        assert!(error.contains("unreviewed.py"), "{error}");
        assert!(error.contains("no baseline"), "{error}");
    }

    #[test]
    fn test_reject_created_file_deletes_working_file() {
        let fixture = ReviewFixture::new();
        let path = fixture.path("created_thing.py");
        fixture.store.record_baseline(&path).unwrap();
        fixture.write("created_thing.py", "content\n");

        fixture
            .store
            .reject_paths(std::slice::from_ref(&path))
            .unwrap();

        assert!(!path.exists());
        assert!(fixture.store.status().unwrap().is_empty());
    }

    #[test]
    fn test_approve_all_and_reject_all() {
        let project = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        let root = project.path().canonicalize().unwrap();
        let store = ReviewStore::start(&root, session_dir.path()).unwrap();
        for (index, relative) in ["one.py", "two.py", "three.py"].iter().enumerate() {
            let path = root.join(relative);
            fs::write(&path, format!("original\n{index}")).unwrap();
            store.record_baseline(&path).unwrap();
            fs::write(&path, format!("changed\n{index}")).unwrap();
        }

        let baseline_count = store.approve_all().unwrap();
        assert_eq!(baseline_count, 3);
        assert!(store.status().unwrap().is_empty());
        for (index, relative) in ["one.py", "two.py", "three.py"].iter().enumerate() {
            assert_eq!(
                fs::read_to_string(root.join(relative)).unwrap(),
                format!("changed\n{index}")
            );
        }

        let store = ReviewStore::start(&root, session_dir.path()).unwrap();
        for relative in ["one.py", "two.py"] {
            let path = root.join(relative);
            store.record_baseline(&path).unwrap();
            fs::write(&path, "second round\n").unwrap();
        }
        let rejected_count = store.reject_all().unwrap();
        assert_eq!(rejected_count, 2);
        assert!(store.status().unwrap().is_empty());
        assert_eq!(
            fs::read_to_string(root.join("one.py")).unwrap(),
            "changed\n0"
        );
    }

    #[test]
    fn test_prunes_empty_baseline_directories() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("a/b/c.txt", "x");
        fixture.store.record_baseline(&path).unwrap();
        fixture
            .store
            .approve_paths(std::slice::from_ref(&path))
            .unwrap();
        assert!(!fixture.store.baselines_dir.join("a/b").exists());
        assert!(!fixture.store.baselines_dir.join("a").exists());
        assert!(fixture.store.baselines_dir.exists());
    }

    #[test]
    fn test_session_restart_refuses_dirty_session_as_fresh_start() {
        let project = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        let root = project.path().canonicalize().unwrap();
        let store = ReviewStore::start(&root, session_dir.path()).unwrap();
        let path = root.join("kept.py");
        fs::write(&path, "x").unwrap();
        store.record_baseline(&path).unwrap();

        let restart_result = ReviewStore::start(&root, session_dir.path());
        let error = restart_result.unwrap_err().to_string();
        assert!(error.contains("already contains baselines"), "{error}");
    }

    #[test]
    fn test_session_restart_on_baselines_cleaned_and_wrong_root_refused() {
        let project = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        let root = project.path().canonicalize().unwrap();
        ReviewStore::start(&root, session_dir.path()).unwrap();

        let other_project = TempDir::new().unwrap();
        let other_root = other_project.path().canonicalize().unwrap();
        let error = ReviewStore::start(&other_root, session_dir.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("was started for trusted root"), "{error}");
    }

    #[test]
    fn test_materialize_review_view_only_contains_changed_files() {
        let fixture = ReviewFixture::new();
        fixture.write(
            "changed.py",
            "original
",
        );
        fixture.write(
            "stale.py",
            "original
",
        );
        let changed_path = fixture.path("changed.py");
        fixture.store.record_baseline(&changed_path).unwrap();
        fixture.write(
            "changed.py",
            "changed
",
        );
        let new_file_path = fixture.path("new.py");
        fixture.store.record_baseline(&new_file_path).unwrap();

        let (_view_dir, changed_statuses) = fixture.store.materialize_review_view().unwrap();
        assert_eq!(changed_statuses.len(), 2);
        assert!(
            changed_statuses
                .iter()
                .any(|status| status.path == *"changed.py" && status.kind == FileKind::Modified)
        );
        assert!(
            changed_statuses
                .iter()
                .any(|status| status.path == *"new.py" && status.kind == FileKind::Created)
        );

        let view_dir = fixture.store.session_dir().join("review-view");
        assert!(view_dir.join("changed.py").is_file());
        assert!(view_dir.join("new.py").is_file());
        assert!(!view_dir.join("stale.py").exists());
        assert_eq!(
            fs::read_to_string(view_dir.join("changed.py")).unwrap(),
            "original\n"
        );
    }

    #[test]
    fn test_materialize_review_view_clears_previous_view() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("one.py", "a\n");
        fixture.store.record_baseline(&path).unwrap();
        let (view_dir, _) = fixture.store.materialize_review_view().unwrap();
        fs::write(view_dir.join("obsolete.py"), "").unwrap();

        let (view_dir, _) = fixture.store.materialize_review_view().unwrap();
        assert!(!view_dir.join("obsolete.py").exists());
    }

    #[test]
    fn test_write_atomically_replaces_content_via_rename() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("atomic.txt", "before");
        write_atomically(&path, "after").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "after");
        assert_eq!(
            fs::read_dir(path.parent().unwrap())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .contains("zed-review-mcp.tmp"))
                .count(),
            0
        );
    }
}
