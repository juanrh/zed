use anyhow::{Context as _, Result, anyhow};
use std::path::{Path, PathBuf};

pub async fn resolve_trusted_root(starting_directory: &Path) -> Result<PathBuf> {
    let output = smol::process::Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(starting_directory.to_path_buf())
        .output()
        .await
        .context("running `git rev-parse --show-toplevel`")?;
    if output.status.success() {
        let root = String::from_utf8(output.stdout)
            .context("`git rev-parse` printed invalid UTF-8")?
            .trim()
            .to_string();
        let root = PathBuf::from(root);
        if root.is_dir() {
            return root
                .canonicalize()
                .context("canonicalizing git worktree root");
        }
    }
    starting_directory.canonicalize().with_context(|| {
        format!(
            "resolving working directory {}",
            starting_directory.display()
        )
    })
}

pub fn confine(path: &Path, trusted_root: &Path) -> Result<PathBuf> {
    anyhow::ensure!(
        path.is_absolute(),
        "path must be absolute: {} (relative paths are ambiguous inside the review session)",
        path.display()
    );
    anyhow::ensure!(
        trusted_root.is_absolute(),
        "trusted root must be absolute: {}",
        trusted_root.display()
    );

    if let Ok(existing) = path.canonicalize() {
        return if existing.starts_with(trusted_root) {
            Ok(existing)
        } else {
            Err(anyhow!(
                "{} is outside the review trusted root {}",
                existing.display(),
                trusted_root.display()
            ))
        };
    }

    let mut existing_ancestor = PathBuf::from(path);
    let mut remaining = Vec::new();
    while !existing_ancestor.exists() {
        let file_name = existing_ancestor
            .file_name()
            .ok_or_else(|| {
                anyhow!(
                    "cannot confine path {}: no existing ancestor",
                    path.display()
                )
            })?
            .to_owned();
        remaining.push(file_name);
        existing_ancestor = existing_ancestor
            .parent()
            .ok_or_else(|| {
                anyhow!(
                    "cannot confine path {}: no existing ancestor",
                    path.display()
                )
            })?
            .to_path_buf();
    }
    let mut confined = existing_ancestor
        .canonicalize()
        .with_context(|| format!("resolving {}", existing_ancestor.display()))?;
    anyhow::ensure!(
        confined.starts_with(trusted_root),
        "{} is outside the review trusted root {}",
        path.display(),
        trusted_root.display()
    );
    for component in remaining.iter().rev() {
        confined.push(component);
    }
    Ok(confined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    fn temp_root() -> (TempDir, PathBuf) {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path().canonicalize().unwrap();
        (temp_dir, root)
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn test_confine_existing_file_inside_root() {
        let (_temp_dir, root) = temp_root();
        let file_path = root.join("src/main.py");
        write(&file_path, "print('hello')\n");
        assert_eq!(confine(&file_path, &root).unwrap(), file_path);
    }

    #[test]
    fn test_confine_missing_file_inside_root() {
        let (_temp_dir, root) = temp_root();
        fs::create_dir_all(root.join("src/nested")).unwrap();
        let file_path = root.join("src/nested/new_file.py");
        let confined = confine(&file_path, &root).unwrap();
        assert_eq!(confined, file_path);
    }

    #[test]
    fn test_confine_missing_parents_inside_root() {
        let (_temp_dir, root) = temp_root();
        let file_path = root.join("a/b/c/new_file.py");
        let confined = confine(&file_path, &root).unwrap();
        assert_eq!(confined, file_path);
    }

    #[test]
    fn test_confine_rejects_relative_path() {
        let (_temp_dir, root) = temp_root();
        assert!(confine(Path::new("relative.py"), &root).is_err());
    }

    fn external_temp_file(contents: &str) -> (TempDir, PathBuf) {
        let other_temp_dir = TempDir::new().unwrap();
        let file_path = other_temp_dir.path().join("outside.py");
        fs::write(&file_path, contents).unwrap();
        (other_temp_dir, file_path)
    }

    #[test]
    fn test_confine_rejects_existing_path_outside_root() {
        let (_temp_dir, root) = temp_root();
        let (_other_temp_dir, outside) = external_temp_file("x\n");
        assert!(confine(&outside, &root).is_err());
    }

    #[test]
    fn test_confine_rejects_missing_path_outside_root() {
        let (_temp_dir, root) = temp_root();
        let other_temp_dir = TempDir::new().unwrap();
        let outside = other_temp_dir.path().join("new/missing.py");
        assert!(confine(&outside, &root).is_err());
    }

    #[test]
    fn test_confine_escaped_symlink_inside_root() {
        let (_temp_dir, root) = temp_root();
        let other_temp_dir = TempDir::new().unwrap();
        let escape = other_temp_dir.path().join("escaped.py");
        fs::write(&escape, "secrets").unwrap();
        symlink(&escape, root.join("inside_link")).unwrap();
        assert!(confine(&root.join("inside_link"), &root).is_err());
    }

    #[test]
    fn test_confine_follows_symlink_to_inside_root() {
        let (_temp_dir, root) = temp_root();
        let target = root.join("src/main.py");
        write(&target, "x = 1\n");
        symlink(&target, root.join("alias.py")).unwrap();
        assert_eq!(confine(&root.join("alias.py"), &root).unwrap(), target);
    }

    #[test]
    fn test_resolve_trusted_root_prefers_git_worktree() {
        smol::block_on(async {
            let temp_dir = TempDir::new().unwrap();
            let script = r#"git config user.email t@t.t; git config user.name t; git init -q .; mkdir -p sub; echo x > sub/file.txt"#;
            smol::process::Command::new("sh")
                .arg("-c")
                .arg(script)
                .current_dir(temp_dir.path())
                .output()
                .await
                .expect("setting up temp git repo");

            let nested = temp_dir.path().join("sub");
            let root = resolve_trusted_root(&nested).await.unwrap();
            assert_eq!(root, temp_dir.path().canonicalize().unwrap());
        })
    }

    #[test]
    fn test_resolve_trusted_root_falls_back_to_working_directory() {
        smol::block_on(async {
            let temp_dir = TempDir::new().unwrap();
            let root = resolve_trusted_root(temp_dir.path()).await.unwrap();
            assert_eq!(root, temp_dir.path().canonicalize().unwrap());
        })
    }
}
