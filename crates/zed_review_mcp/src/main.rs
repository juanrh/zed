use anyhow::{Context as _, Result, anyhow, bail};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zed_review_mcp::ReviewStore;
use zed_review_mcp::review_server::{ReviewServer, serve_stdio};
use zed_review_mcp::trusted_root::resolve_trusted_root;
use zed_review_mcp::zed_opener::ZedOpener;

const USAGE: &str = "zed-review-mcp: an MCP server that gives external coding agents \
Zed's asynchronous change-review workflow (baselines + rich diff review in the editor).

The server speaks MCP over stdin/stdout. It resolves the git worktree root of the \
current directory as the review trusted root and keeps baseline snapshots in a \
session directory.

Usage:
  zed-review-mcp [--session-dir <directory>]

Environment:
  ZED_REVIEW_MCP_SESSION_DIR   session directory (overridden by --session-dir)
  ZED_PATH                     path to the zed executable (defaults to `zed` on PATH)";

#[tokio::main]
async fn main() -> Result<()> {
    let (session_directory, cleanup_session) = resolve_session_directory(std::env::args().skip(1))?;
    let working_directory = std::env::current_dir().context("resolving the current directory")?;
    let trusted_root = resolve_trusted_root(&working_directory)
        .await
        .context("resolving the review trusted root")?;
    let zed_opener = ZedOpener::resolve()
        .map(Arc::new)
        .map_err(|error| anyhow!("{error:#}"))?;
    anyhow::ensure!(
        trusted_root.is_dir(),
        "review trusted root {} is not a directory",
        trusted_root.display()
    );

    let review_store = ReviewStore::start(&trusted_root, &session_directory)?;
    let result = serve_stdio(ReviewServer::new(
        Arc::new(review_store),
        zed_opener.clone(),
    ))
    .await;

    if cleanup_session {
        if let Err(cleanup_error) = std::fs::remove_dir_all(&session_directory) {
            eprintln!(
                "zed-review-mcp: could not clean up session directory {}: {cleanup_error}",
                session_directory.display()
            );
        }
    }
    result
}

fn resolve_session_directory(arguments: impl Iterator<Item = String>) -> Result<(PathBuf, bool)> {
    let arguments: Vec<String> = arguments.collect();
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("{USAGE}");
        std::process::exit(0);
    }
    if let Some(index) = arguments
        .iter()
        .position(|argument| argument == "--session-dir")
    {
        let directory = arguments
            .get(index + 1)
            .context("--session-dir requires a value")?;
        let unexpected_remainder = arguments.iter().skip(index + 2).next();
        anyhow::ensure!(
            unexpected_remainder.is_none(),
            "unexpected argument: {}\n\n{USAGE}",
            unexpected_remainder.map(String::as_str).unwrap_or_default()
        );
        return Ok((PathBuf::from(directory), false));
    }
    if let Some(unexpected) = arguments.first() {
        bail!("unexpected argument: {unexpected}\n\n{USAGE}");
    }
    if let Some(directory) = std::env::var_os("ZED_REVIEW_MCP_SESSION_DIR") {
        if !directory.is_empty() {
            return Ok((PathBuf::from(directory), false));
        }
    }
    Ok((default_session_directory(), true))
}

fn default_session_directory() -> PathBuf {
    timestamped_session_directory(&std::env::temp_dir(), std::process::id())
}

fn timestamped_session_directory(base: &Path, process_id: u32) -> PathBuf {
    base.join(format!("zed-review-mcp-{process_id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_session_directory_is_deterministic_per_process() {
        let base = Path::new("/tmp");
        assert_eq!(
            timestamped_session_directory(base, 42),
            base.join("zed-review-mcp-42")
        );
    }

    #[test]
    fn test_resolve_session_directory_rejects_unknown_flags() {
        assert!(resolve_session_directory(["--nonsense".to_string()].into_iter()).is_err());
    }

    #[test]
    fn test_resolve_session_directory_requires_a_value() {
        assert!(resolve_session_directory(["--session-dir".to_string()].into_iter()).is_err());
    }
}
