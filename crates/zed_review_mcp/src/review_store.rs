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

#[derive(Debug)]
enum ReviewEntry {
    Snapshot {
        relative: PathBuf,
        snapshot_path: PathBuf,
    },
    CreationMarker {
        relative: PathBuf,
        marker_path: PathBuf,
    },
}

impl ReviewEntry {
    fn relative(&self) -> &Path {
        match self {
            ReviewEntry::Snapshot { relative, .. } => relative,
            ReviewEntry::CreationMarker { relative, .. } => relative,
        }
    }
}

#[derive(Debug)]
pub struct ReviewStore {
    trusted_root: PathBuf,
    session_dir: PathBuf,
    baselines_dir: PathBuf,
    creations_dir: PathBuf,
    workspace_lock: Mutex<()>,
}

const TRUSTED_ROOT_FILE: &str = "trusted-root";
pub const BASELINES_DIR: &str = "baselines";
pub const CREATIONS_DIR: &str = "creations";
pub const REVIEW_BASELINE_VIEW_DIR: &str = "review-baseline-view";
pub const REVIEW_WORK_VIEW_DIR: &str = "review-work-view";

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
        let creations_dir = session_dir.join(CREATIONS_DIR);

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
                baselines_dir.is_dir() && creations_dir.is_dir(),
                "session directory {} is missing its review directories",
                session_dir.display()
            );
            anyhow::ensure!(
                !directory_contains_any_entry(&baselines_dir)?
                    && !directory_contains_any_entry(&creations_dir)?,
                "session directory {} already contains baselines for another review; \
                 start a fresh session for a new review round",
                session_dir.display()
            );
        } else {
            fs::create_dir_all(&baselines_dir)
                .with_context(|| format!("creating {}", baselines_dir.display()))?;
            fs::create_dir_all(&creations_dir)
                .with_context(|| format!("creating {}", creations_dir.display()))?;
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
            creations_dir,
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

    pub fn creations_dir(&self) -> &Path {
        &self.creations_dir
    }

    fn relative_to_trusted_root(&self, path: &Path) -> Result<PathBuf> {
        path.strip_prefix(&self.trusted_root)
            .map(PathBuf::from)
            .with_context(|| format!("{} is not under the trusted root", path.display()))
    }

    fn snapshot_path_for(&self, path: &Path) -> Result<PathBuf> {
        let confined = confine(path, &self.trusted_root)?;
        let relative = self.relative_to_trusted_root(&confined)?;
        Ok(self.baselines_dir.join(relative))
    }

    fn creation_marker_path_for(&self, path: &Path) -> Result<PathBuf> {
        let confined = confine(path, &self.trusted_root)?;
        let relative = self.relative_to_trusted_root(&confined)?;
        Ok(self.creations_dir.join(relative))
    }

    fn review_entry_for(&self, path: &Path) -> Result<Option<ReviewEntry>> {
        let snapshot_path = self.snapshot_path_for(path)?;
        if snapshot_path.is_file() {
            let relative = snapshot_path
                .strip_prefix(&self.baselines_dir)
                .context("baseline snapshot outside baselines dir")
                .map(PathBuf::from)?;
            return Ok(Some(ReviewEntry::Snapshot {
                relative,
                snapshot_path,
            }));
        }
        let marker_path = self.creation_marker_path_for(path)?;
        if marker_path.is_file() {
            let relative = marker_path
                .strip_prefix(&self.creations_dir)
                .context("creation marker outside creations dir")
                .map(PathBuf::from)?;
            return Ok(Some(ReviewEntry::CreationMarker {
                relative,
                marker_path,
            }));
        }
        Ok(None)
    }

    fn review_entries(&self) -> Result<Vec<ReviewEntry>> {
        let mut entries = Vec::new();
        let mut stack = vec![(self.baselines_dir.clone(), true)];
        stack.push((self.creations_dir.clone(), false));
        while let Some((directory, is_snapshot_tree)) = stack.pop() {
            let directory_entries = fs::read_dir(&directory)
                .with_context(|| format!("reading {}", directory.display()))?;
            for entry in directory_entries {
                let entry = entry.with_context(|| format!("reading {}", directory.display()))?;
                let entry_path = entry.path();
                let file_type = entry
                    .file_type()
                    .with_context(|| format!("reading {}", entry_path.display()))?;
                if file_type.is_dir() {
                    stack.push((entry_path, is_snapshot_tree));
                } else if file_type.is_file() {
                    let directory = if is_snapshot_tree {
                        &self.baselines_dir
                    } else {
                        &self.creations_dir
                    };
                    let relative = entry_path
                        .strip_prefix(directory)
                        .context("review entry outside its tree")
                        .map(PathBuf::from)?;
                    if is_snapshot_tree {
                        entries.push(ReviewEntry::Snapshot {
                            relative,
                            snapshot_path: entry_path,
                        });
                    } else {
                        entries.push(ReviewEntry::CreationMarker {
                            relative,
                            marker_path: entry_path,
                        });
                    }
                }
            }
        }
        entries.sort_by(|left, right| left.relative().cmp(right.relative()));
        Ok(entries)
    }

    fn remove_entry(&self, entry_path: &Path, tree_root: &Path) -> Result<()> {
        fs::remove_file(entry_path)
            .with_context(|| format!("removing {}", entry_path.display()))?;
        let mut directory = entry_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        while directory.starts_with(tree_root) && directory != tree_root {
            if fs::remove_dir(&directory).is_err() {
                return Ok(());
            }
            directory.pop();
        }
        Ok(())
    }

    pub fn has_baseline(&self, path: &Path) -> Result<bool> {
        Ok(self.review_entry_for(path)?.is_some())
    }

    pub fn record_baseline(&self, path: &Path) -> Result<RecordOutcome> {
        let _guard = self
            .workspace_lock
            .lock()
            .map_err(|_| anyhow!("review store workspace lock poisoned"))?;
        anyhow::ensure!(
            self.review_entry_for(path)?.is_none(),
            "{path} is already under review (it has a baseline from this round); \
             approve or reject it before recording a new baseline",
            path = path.display()
        );
        match path.canonicalize() {
            Ok(existing) => {
                let metadata = fs::metadata(&existing)
                    .with_context(|| format!("reading {}", existing.display()))?;
                anyhow::ensure!(
                    metadata.is_file(),
                    "{path} is a directory; only files can be reviewed",
                    path = path.display()
                );
                let contents = fs::read(&existing)
                    .with_context(|| format!("reading {}", existing.display()))?;
                let contents = String::from_utf8(contents)
                    .map_err(|_| {
                        anyhow!(
                            "{path} is not a UTF-8 text file; only text files can be reviewed",
                            path = path.display()
                        )
                    })?
                    .into_bytes();
                let snapshot_path = self.snapshot_path_for(path)?;
                if let Some(parent) = snapshot_path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("creating {}", parent.display()))?;
                }
                fs::write(&snapshot_path, contents).with_context(|| {
                    format!("writing baseline snapshot {}", snapshot_path.display())
                })?;
                Ok(RecordOutcome::Recorded)
            }
            Err(_) => {
                let marker_path = self.creation_marker_path_for(path)?;
                if let Some(parent) = marker_path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("creating {}", parent.display()))?;
                }
                fs::write(&marker_path, b"").with_context(|| {
                    format!("writing creation marker {}", marker_path.display())
                })?;
                Ok(RecordOutcome::MarkedForCreation)
            }
        }
    }

    pub fn status(&self) -> Result<Vec<FileStatus>> {
        let _guard = self
            .workspace_lock
            .lock()
            .map_err(|_| anyhow!("review store workspace lock poisoned"))?;
        let mut statuses = Vec::new();
        for entry in self.review_entries()? {
            let working_path = self.trusted_root.join(entry.relative());
            let kind = match &entry {
                ReviewEntry::CreationMarker { .. } => {
                    if working_path.exists() && !working_path.is_file() {
                        anyhow::bail!(
                            "{working_path} exists but is not a file; \
                             the creation baseline cannot be compared",
                            working_path = working_path.display()
                        );
                    }
                    FileKind::Created
                }
                ReviewEntry::Snapshot { snapshot_path, .. } => {
                    if !working_path.exists() {
                        FileKind::Removed
                    } else if fs::read(snapshot_path)? == fs::read(&working_path)? {
                        FileKind::Unchanged
                    } else {
                        FileKind::Modified
                    }
                }
            };
            statuses.push(FileStatus {
                path: entry.relative().to_path_buf(),
                kind,
            });
        }
        Ok(statuses)
    }

    fn reviewed_paths(&self) -> Result<Vec<PathBuf>> {
        Ok(self
            .review_entries()?
            .into_iter()
            .map(|entry| self.trusted_root.join(entry.relative()))
            .collect())
    }

    pub fn approve_all(&self) -> Result<usize> {
        let _guard = self
            .workspace_lock
            .lock()
            .map_err(|_| anyhow!("review store workspace lock poisoned"))?;
        let entries = self.review_entries()?;
        let approved_count = entries.len();
        for entry in entries {
            match entry {
                ReviewEntry::Snapshot { snapshot_path, .. } => {
                    self.remove_entry(&snapshot_path, &self.baselines_dir)?
                }
                ReviewEntry::CreationMarker { marker_path, .. } => {
                    self.remove_entry(&marker_path, &self.creations_dir)?
                }
            }
        }
        Ok(approved_count)
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
        let mut approved_count = 0;
        for path in paths {
            let entry = self.review_entry_for(path)?.with_context(|| {
                anyhow!("{} has no baseline; it is not under review", path.display())
            })?;
            match entry {
                ReviewEntry::Snapshot { snapshot_path, .. } => {
                    self.remove_entry(&snapshot_path, &self.baselines_dir)?
                }
                ReviewEntry::CreationMarker { marker_path, .. } => {
                    self.remove_entry(&marker_path, &self.creations_dir)?
                }
            }
            approved_count += 1;
        }
        Ok(approved_count)
    }

    pub fn approve(&self, path: &Path) -> Result<usize> {
        self.approve_paths(&[path.to_path_buf()])
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

    pub fn reject(&self, path: &Path) -> Result<usize> {
        self.reject_paths(&[path.to_path_buf()])
    }

    fn reject_targets(&self, targets: Vec<PathBuf>) -> Result<usize> {
        let mut rejected_count = 0;
        for target in targets {
            let entry = self.review_entry_for(&target)?.with_context(|| {
                anyhow!(
                    "{} has no baseline; it is not under review",
                    target.display()
                )
            })?;
            match entry {
                ReviewEntry::CreationMarker { relative, .. } => {
                    let working_path = self.trusted_root.join(&relative);
                    if working_path.exists() {
                        anyhow::ensure!(
                            working_path.is_file(),
                            "{working_path} is a directory; \
                             rejecting a created file cannot remove a directory",
                            working_path = working_path.display()
                        );
                        fs::remove_file(&working_path).with_context(|| {
                            format!("removing created file {}", working_path.display())
                        })?;
                    }
                    let marker_path = self.creations_dir.join(&relative);
                    self.remove_entry(&marker_path, &self.creations_dir)?;
                }
                ReviewEntry::Snapshot {
                    relative,
                    snapshot_path,
                } => {
                    let baseline_contents = fs::read(&snapshot_path).with_context(|| {
                        format!("reading baseline snapshot {}", snapshot_path.display())
                    })?;
                    let contents = String::from_utf8(baseline_contents).map_err(|_| {
                        anyhow!(
                            "baseline snapshot {} is not valid UTF-8",
                            snapshot_path.display()
                        )
                    })?;
                    write_atomically(&self.trusted_root.join(&relative), &contents)?;
                    self.remove_entry(&snapshot_path, &self.baselines_dir)?;
                }
            }
            rejected_count += 1;
        }
        Ok(rejected_count)
    }

    pub fn materialize_review_views(&self) -> Result<(PathBuf, PathBuf, Vec<FileStatus>)> {
        let baseline_view_dir = self.session_dir.join(REVIEW_BASELINE_VIEW_DIR);
        let work_view_dir = self.session_dir.join(REVIEW_WORK_VIEW_DIR);
        for view_dir in [&baseline_view_dir, &work_view_dir] {
            if view_dir.exists() {
                fs::remove_dir_all(view_dir)
                    .with_context(|| format!("clearing {}", view_dir.display()))?;
            }
            fs::create_dir_all(view_dir)
                .with_context(|| format!("creating {}", view_dir.display()))?;
        }

        let mut changed_statuses = Vec::new();
        for file_status in self.status()? {
            if file_status.kind == FileKind::Unchanged {
                continue;
            }
            let baseline_view_path = baseline_view_dir.join(&file_status.path);
            if let Some(parent) = baseline_view_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            match self.review_entry_for(&self.trusted_root.join(&file_status.path))? {
                Some(ReviewEntry::Snapshot { snapshot_path, .. }) => {
                    fs::copy(&snapshot_path, &baseline_view_path).with_context(|| {
                        format!(
                            "copying {} to {}",
                            snapshot_path.display(),
                            baseline_view_path.display()
                        )
                    })?;
                }
                Some(ReviewEntry::CreationMarker { .. }) => {
                    fs::write(&baseline_view_path, b"")
                        .with_context(|| format!("writing {}", baseline_view_path.display()))?;
                }
                None => anyhow::bail!(
                    "{path} drifted but has no review entry",
                    path = file_status.path.display()
                ),
            }
            let working_path = self.trusted_root.join(&file_status.path);
            if working_path.is_file() {
                let work_view_path = work_view_dir.join(&file_status.path);
                if let Some(parent) = work_view_path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("creating {}", parent.display()))?;
                }
                fs::copy(&working_path, &work_view_path).with_context(|| {
                    format!(
                        "copying {} to {}",
                        working_path.display(),
                        work_view_path.display()
                    )
                })?;
            }
            changed_statuses.push(file_status);
        }
        Ok((baseline_view_dir, work_view_dir, changed_statuses))
    }
}

fn directory_contains_any_entry(directory: &Path) -> Result<bool> {
    let entries =
        fs::read_dir(directory).with_context(|| format!("opening {}", directory.display()))?;
    Ok(entries.filter_map(|entry| entry.ok()).next().is_some())
}

pub fn write_atomically(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let temporary_path = parent.join(format!(".{file_name}.zed-review-mcp.tmp"));
    let mut attempt = 0;
    let mut temporary_path = temporary_path;
    while temporary_path.exists() {
        attempt += 1;
        temporary_path = parent.join(format!(".{file_name}.zed-review-mcp.tmp.{attempt}"));
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

        fixture.store.approve(&path).unwrap();
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
    fn test_existing_empty_file_is_a_snapshot_not_a_creation_marker() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("empty.txt", "");
        assert_eq!(
            fixture.store.record_baseline(&path).unwrap(),
            RecordOutcome::Recorded
        );
        assert_eq!(
            file_status_kinds(&fixture.store.status().unwrap()),
            vec![("empty.txt".into(), FileKind::Unchanged)]
        );

        fixture.write("empty.txt", "content added later\n");
        assert_eq!(
            file_status_kinds(&fixture.store.status().unwrap()),
            vec![("empty.txt".into(), FileKind::Modified)]
        );

        fixture.store.reject(&path).unwrap();
        assert!(path.is_file());
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn test_re_record_rejected_with_offending_path() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("main.py", "print('hi')\n");
        fixture.store.record_baseline(&path).unwrap();
        let error = fixture
            .store
            .record_baseline(&path)
            .unwrap_err()
            .to_string();
        assert!(error.contains("main.py"), "{error}");
        assert!(error.contains("already under review"), "{error}");
    }

    #[test]
    fn test_record_rejects_directory() {
        let fixture = ReviewFixture::new();
        fixture.write("nested/inner.txt", "x");
        let error = fixture
            .store
            .record_baseline(&fixture.root.join("nested").canonicalize().unwrap())
            .unwrap_err();
        assert!(error.to_string().contains("nested"), "{error}");
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
        let link = fixture.root.join("link.py");
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

        fixture.store.reject(&path).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "print('original')\n");
        assert!(fixture.store.status().unwrap().is_empty());
    }

    #[test]
    fn test_reject_restores_missing_parent_directory_file() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("a/b/file.txt", "one\n");
        fixture.store.record_baseline(&path).unwrap();
        fs::remove_dir_all(fixture.root.join("a")).unwrap();

        fixture.store.reject(&path).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "one\n");
        assert!(fixture.store.status().unwrap().is_empty());
    }

    #[test]
    fn test_reject_errors_when_no_baseline() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("wrong.py", "x");
        let error = fixture.store.reject(&path).unwrap_err().to_string();
        assert!(error.contains("wrong.py"), "{error}");
        assert!(error.contains("no baseline"), "{error}");
    }

    #[test]
    fn test_approve_keeps_working_tree_and_ends_review() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("main.py", "a\n");
        fixture.store.record_baseline(&path).unwrap();
        fixture.write("main.py", "b\n");

        fixture.store.approve(&path).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "b\n");
        assert!(fixture.store.status().unwrap().is_empty());
    }

    #[test]
    fn test_approve_errors_when_no_baseline() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("unreviewed.py", "x");
        let error = fixture.store.approve(&path).unwrap_err().to_string();
        assert!(error.contains("unreviewed.py"), "{error}");
        assert!(error.contains("no baseline"), "{error}");
    }

    #[test]
    fn test_reject_created_file_deletes_working_file() {
        let fixture = ReviewFixture::new();
        let path = fixture.path("created_thing.py");
        fixture.store.record_baseline(&path).unwrap();
        fixture.write("created_thing.py", "content\n");

        fixture.store.reject(&path).unwrap();

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
        fixture.store.approve(&path).unwrap();
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

        let error = ReviewStore::start(&root, session_dir.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("already contains baselines"), "{error}");
    }

    #[test]
    fn test_session_restart_on_cleaned_session_and_wrong_root_refused() {
        let project = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        let root = project.path().canonicalize().unwrap();
        ReviewStore::start(&root, session_dir.path()).unwrap();
        ReviewStore::start(&root, session_dir.path()).unwrap();

        let other_project = TempDir::new().unwrap();
        let other_root = other_project.path().canonicalize().unwrap();
        let error = ReviewStore::start(&other_root, session_dir.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("was started for trusted root"), "{error}");
    }

    #[test]
    fn test_materialize_review_views_only_contains_changed_files() {
        let fixture = ReviewFixture::new();
        let changed_path = fixture.write("changed.py", "original\n");
        fixture.write("stale.py", "original\n");
        fixture.store.record_baseline(&changed_path).unwrap();
        fixture.write("changed.py", "changed\n");
        let new_file_path = fixture.path("new.py");
        fixture.store.record_baseline(&new_file_path).unwrap();
        fixture.write("new.py", "brand new\n");
        let removed_path = fixture.write("doomed.py", "delete me\n");
        fixture.store.record_baseline(&removed_path).unwrap();
        fs::remove_file(&removed_path).unwrap();

        let (baseline_view_dir, work_view_dir, changed_statuses) =
            fixture.store.materialize_review_views().unwrap();
        assert_eq!(changed_statuses.len(), 3);
        assert_eq!(
            file_status_kinds(&changed_statuses),
            vec![
                ("changed.py".into(), FileKind::Modified),
                ("doomed.py".into(), FileKind::Removed),
                ("new.py".into(), FileKind::Created),
            ]
        );

        assert!(baseline_view_dir.join("changed.py").is_file());
        assert!(work_view_dir.join("changed.py").is_file());
        assert_eq!(
            fs::read_to_string(baseline_view_dir.join("changed.py")).unwrap(),
            "original\n"
        );
        assert_eq!(
            fs::read_to_string(work_view_dir.join("changed.py")).unwrap(),
            "changed\n"
        );
        assert_eq!(fs::read(baseline_view_dir.join("new.py")).unwrap().len(), 0);
        assert_eq!(
            fs::read_to_string(work_view_dir.join("new.py")).unwrap(),
            "brand new\n"
        );
        assert!(baseline_view_dir.join("doomed.py").is_file());
        assert!(!work_view_dir.join("doomed.py").exists());
        assert!(!baseline_view_dir.join("stale.py").exists());
        assert!(!work_view_dir.join("stale.py").exists());
    }

    #[test]
    fn test_materialize_review_views_clears_previous_view() {
        let fixture = ReviewFixture::new();
        let path = fixture.write("one.py", "a\n");
        fixture.store.record_baseline(&path).unwrap();
        let (baseline_view_dir, _work_view_dir, _) =
            fixture.store.materialize_review_views().unwrap();
        fs::write(baseline_view_dir.join("obsolete.py"), "").unwrap();

        let (baseline_view_dir, _work_view_dir, _) =
            fixture.store.materialize_review_views().unwrap();
        assert!(!baseline_view_dir.join("obsolete.py").exists());
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
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .contains("zed-review-mcp.tmp")
                })
                .count(),
            0
        );
    }
}
