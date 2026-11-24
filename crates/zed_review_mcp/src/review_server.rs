use crate::review_store::{RecordOutcome, ReviewStore};
use crate::zed_opener::ZedOpener;
use anyhow::{Context as _, Error, Result};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities, ServerConfig,
};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const SERVER_NAME: &str = "zed-review-mcp";

pub struct ReviewServer {
    review_store: Arc<ReviewStore>,
    zed_opener: Arc<ZedOpener>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct RecordBaselineArgs {
    /// Absolute paths of files to put under review. Call this BEFORE editing each file.
    /// Files that do not exist yet are recorded as marked for creation.
    pub paths: Vec<String>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct DecisionArgs {
    /// Absolute paths to approve or reject. Omit to decide every file under review.
    pub paths: Option<Vec<String>>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct EditArgs {
    /// Absolute path of the file to edit.
    pub file_path: String,
    /// The exact existing text to replace.
    pub old_string: String,
    /// The replacement text.
    pub new_string: String,
    /// Replace every occurrence of old_string. Omit when old_string is unique.
    pub replace_all: Option<bool>,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct OpenPathsArgs {
    /// Absolute paths to open in the running Zed.
    pub paths: Vec<String>,
}

fn parse_absolute_paths(paths: &[String]) -> Result<Vec<PathBuf>> {
    paths
        .iter()
        .map(|path| {
            let path = PathBuf::from(path);
            anyhow::ensure!(
                path.is_absolute(),
                "path must be absolute: {} (relative paths are ambiguous inside the review session)",
                path.display()
            );
            Ok(path)
        })
        .collect()
}

fn acceptance_paths(paths: Option<Vec<String>>) -> Result<Option<Vec<PathBuf>>> {
    paths
        .map(|paths| {
            let parsed_paths = parse_absolute_paths(&paths)?;
            anyhow::ensure!(
                !parsed_paths.is_empty(),
                "when paths are given, at least one is required (pass no paths to decide every file under review)"
            );
            Ok(parsed_paths)
        })
        .transpose()
}

fn error_result(error: Error) -> CallToolResult {
    let message = format!("{error:#}");
    log::error!("tool call failed: {message}");
    CallToolResult::error(vec![ContentBlock::text(message)])
}

fn run_tool(
    tool_call: impl FnOnce() -> Result<CallToolResult>,
) -> Result<CallToolResult, McpError> {
    Ok(tool_call().unwrap_or_else(error_result))
}

async fn run_tool_async<F>(tool_call: F) -> Result<CallToolResult, McpError>
where
    F: std::future::Future<Output = Result<CallToolResult>>,
{
    Ok(tool_call.await.unwrap_or_else(error_result))
}

impl ReviewServer {
    pub fn new(review_store: Arc<ReviewStore>, zed_opener: Arc<ZedOpener>) -> Self {
        Self {
            review_store,
            zed_opener,
            tool_router: Self::tool_router(),
        }
    }

    pub fn review_store(&self) -> &ReviewStore {
        &self.review_store
    }
}

fn apply_edit(
    review_store: &ReviewStore,
    file_path: &Path,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<(bool, String)> {
    anyhow::ensure!(!old_string.is_empty(), "old_string must not be empty");
    anyhow::ensure!(
        old_string != new_string,
        "old_string and new_string are identical; an edit must change the file"
    );
    anyhow::ensure!(
        file_path.is_file(),
        "{} does not exist; create the file before editing it",
        file_path.display()
    );
    let contents = std::fs::read_to_string(file_path)
        .with_context(|| format!("reading {}", file_path.display()))?;
    let match_count = contents.match_indices(old_string).count();
    match match_count {
        0 => anyhow::bail!("old_string was not found in {}", file_path.display()),
        _ if !replace_all && match_count > 1 => anyhow::bail!(
            "old_string occurs {match_count} times in {}; make it unique or pass replace_all",
            file_path.display()
        ),
        _ => {}
    }
    let baseline_registered = !review_store.has_baseline(file_path)?;
    if baseline_registered {
        review_store.record_baseline(file_path)?;
    }
    let edited_contents = if replace_all {
        contents.replace(old_string, new_string)
    } else {
        contents.replacen(old_string, new_string, 1)
    };
    crate::review_store::write_atomically(file_path, &edited_contents)?;
    Ok((baseline_registered, edited_contents))
}

#[tool_router]
impl ReviewServer {
    #[tool(
        description = "Record review baselines for files BEFORE editing them. \
        The current content of each file is snapshotted; files that do not exist yet are \
        recorded as marked for creation. Do not record a file that is already under review. \
        After editing, call open_review to show the user the diff in Zed, and poll \
        get_change_status for live drift."
    )]
    fn record_baseline(
        &self,
        Parameters(RecordBaselineArgs { paths }): Parameters<RecordBaselineArgs>,
    ) -> Result<CallToolResult, McpError> {
        run_tool(|| {
            anyhow::ensure!(
                !paths.is_empty(),
                "record_baseline requires at least one path"
            );
            let parsed_paths = parse_absolute_paths(&paths)?;
            let mut recorded_paths = Vec::new();
            let mut created_paths = Vec::new();
            for path in &parsed_paths {
                match self.review_store.record_baseline(path)? {
                    RecordOutcome::Recorded => {
                        recorded_paths.push(path.to_string_lossy().to_string())
                    }
                    RecordOutcome::MarkedForCreation => {
                        created_paths.push(path.to_string_lossy().to_string())
                    }
                }
            }
            let mut message = String::from("baselines recorded\n");
            if !recorded_paths.is_empty() {
                message.push_str(&format!("  recorded: {}\n", recorded_paths.join(", ")));
            }
            if !created_paths.is_empty() {
                message.push_str(&format!(
                    "  marked for creation: {}\n",
                    created_paths.join(", ")
                ));
            }
            message
                .push_str("edit the files, then call open_review to show the user the diff in Zed");
            Ok(CallToolResult::success(vec![ContentBlock::text(message)]))
        })
    }

    #[tool(
        description = "Report the live review status: for every file under review, \
        whether it is unchanged, modified, created or removed versus its baseline. \
        Poll this at any time; it never blocks."
    )]
    fn get_change_status(&self) -> Result<CallToolResult, McpError> {
        run_tool(|| {
            let statuses = self.review_store.status()?;
            if statuses.is_empty() {
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    "no files are under review",
                )]));
            }
            let mut message = format!("{} file(s) under review\n", statuses.len());
            for file_status in &statuses {
                message.push_str(&format!(
                    "  {}: {}\n",
                    file_status.path.display(),
                    file_status.kind
                ));
            }
            Ok(CallToolResult::success(vec![ContentBlock::text(message)]))
        })
    }

    #[tool(
        description = "Edit a file with an exact string replacement and automatically \
        record a review baseline on the first edit of each file (a baseline-aware drop-in \
        replacement for the built-in edit tool: unique match unless replace_all, and \
        old_string and new_string must differ). Prefer this tool over editing files with \
        shell commands during a review session."
    )]
    async fn edit(
        &self,
        Parameters(EditArgs {
            file_path,
            old_string,
            new_string,
            replace_all,
        }): Parameters<EditArgs>,
    ) -> Result<CallToolResult, McpError> {
        run_tool_async(async {
            let file_path = PathBuf::from(file_path);
            anyhow::ensure!(
                file_path.is_absolute(),
                "file_path must be absolute: {}",
                file_path.display()
            );
            let (baseline_registered, _edited_contents) = apply_edit(
                &self.review_store,
                &file_path,
                &old_string,
                &new_string,
                replace_all.unwrap_or(false),
            )?;
            let mut message = String::from("edited ");
            message.push_str(&file_path.to_string_lossy());
            message.push('\n');
            if baseline_registered {
                message.push_str(
                    "  baseline registered (first edit of this file in this review round)\n",
                );
            }
            message.push_str("  call open_review to show the user the diff in Zed");
            Ok(CallToolResult::success(vec![ContentBlock::text(message)]))
        })
        .await
    }

    #[tool(
        description = "Open the human review view: Zed shows a rich multi-file diff of \
        everything that drifted from the baselines. Call this once the user should review \
        the pending changes; it opens (or focuses) the running Zed instance."
    )]
    async fn open_review(&self) -> Result<CallToolResult, McpError> {
        run_tool_async(async {
            let (baseline_view_dir, work_view_dir, changed_statuses) =
                self.review_store.materialize_review_views()?;
            if changed_statuses.is_empty() {
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    "no reviewed files have drifted from their baselines yet; edit files, \
                     then call open_review again",
                )]));
            }
            self.zed_opener
                .open_diff(&baseline_view_dir, &work_view_dir)
                .await?;
            let mut message = format!(
                "opened review diff ({} changed file(s)) in Zed\n",
                changed_statuses.len()
            );
            for file_status in &changed_statuses {
                message.push_str(&format!(
                    "  {}: {}\n",
                    file_status.path.display(),
                    file_status.kind
                ));
            }
            message.push_str(
                "the user reviews in Zed, can edit there, and can run linters and tests \
                 before deciding; the decision arrives through approve or reject",
            );
            Ok(CallToolResult::success(vec![ContentBlock::text(message)]))
        })
        .await
    }

    #[tool(
        description = "Open files in the running Zed editor (basic IDE integration, for \
        example to point the user at a specific file)."
    )]
    async fn open_paths(
        &self,
        Parameters(OpenPathsArgs { paths }): Parameters<OpenPathsArgs>,
    ) -> Result<CallToolResult, McpError> {
        run_tool_async(async {
            let parsed_paths = parse_absolute_paths(&paths)?;
            self.zed_opener.open_paths(&parsed_paths).await?;
            Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "opened {} file(s) in Zed",
                parsed_paths.len()
            ))]))
        })
        .await
    }

    #[tool(
        description = "Approve reviewed files: adopt the working tree content as decided, \
        ending review for those paths. The user accepts the changes. Pass no paths to \
        approve every file under review."
    )]
    fn approve(
        &self,
        Parameters(DecisionArgs { paths }): Parameters<DecisionArgs>,
    ) -> Result<CallToolResult, McpError> {
        run_tool(|| {
            let decided_paths = acceptance_paths(paths)?;
            match &decided_paths {
                Some(paths) => self.review_store.approve_paths(paths)?,
                None => self.review_store.approve_all()?,
            };
            let message = format!(
                "approved (working tree kept): {}",
                paths_listing_pathful(&decided_paths)
            );
            Ok(CallToolResult::success(vec![ContentBlock::text(message)]))
        })
    }

    #[tool(
        description = "Reject reviewed files: restore the pre-change baseline content on \
        disk (created files are deleted), ending review for those paths. The user declines \
        the changes. Pass no paths to reject every file under review."
    )]
    fn reject(
        &self,
        Parameters(DecisionArgs { paths }): Parameters<DecisionArgs>,
    ) -> Result<CallToolResult, McpError> {
        run_tool(|| {
            let decided_paths = acceptance_paths(paths)?;
            match &decided_paths {
                Some(paths) => self.review_store.reject_paths(paths)?,
                None => self.review_store.reject_all()?,
            };
            let message = format!(
                "rejected (working tree restored): {}",
                paths_listing_pathful(&decided_paths)
            );
            Ok(CallToolResult::success(vec![ContentBlock::text(message)]))
        })
    }
}

fn paths_listing_pathful(paths: &Option<Vec<PathBuf>>) -> String {
    paths
        .as_ref()
        .map(|paths| {
            paths
                .iter()
                .map(|path| path.to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_else(|| "all files under review".to_string())
}

#[tool_handler]
impl ServerHandler for ReviewServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
            .with_protocol_version(ProtocolVersion::V_2025_06_18)
            .with_instructions(
                "This server gives coding agents a Zed-style asynchronous change review. \
                 Workflow: (1) record_baseline for every file BEFORE editing it, \
                 (2) edit files on disk normally and run linters and tests, \
                 (3) call open_review so the human can review the rich diff in Zed, \
                 (4) poll get_change_status, (5) the user's decision arrives as approve \
                 (keep changes) or reject (restore baseline)."
                    .to_string(),
            )
    }
}

pub async fn serve_stdio(review_server: ReviewServer) -> Result<()> {
    let service = review_server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|error| anyhow::anyhow!("serving MCP over stdio: {error}"))?;
    service
        .waiting()
        .await
        .map_err(|error| anyhow::anyhow!("waiting for the MCP stdio service to finish: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_absolute_paths_rejects_relative() {
        assert!(parse_absolute_paths(&["relative.py".to_string()]).is_err());
        assert!(parse_absolute_paths(&["/tmp/a.py".to_string()]).is_ok());
    }

    #[test]
    fn test_acceptance_paths_defaults_to_all() {
        assert!(acceptance_paths(None).unwrap().is_none());
        assert!(acceptance_paths(Some(vec![])).is_err());
        assert_eq!(
            acceptance_paths(Some(vec!["/tmp/a.py".to_string()]))
                .unwrap()
                .unwrap(),
            vec![PathBuf::from("/tmp/a.py")]
        );
    }

    #[test]
    fn test_error_result_reports_is_error() {
        let result = error_result(anyhow::anyhow!("boom /tmp/x.py"));
        assert!(result.is_error.unwrap_or(true));
    }

    #[test]
    fn test_paths_listing_names_paths_and_all_fallback() {
        let none: Option<Vec<PathBuf>> = None;
        assert_eq!(
            paths_listing_pathful(&none),
            "all files under review".to_string()
        );
        let some = Some(vec![PathBuf::from("/tmp/a.py"), PathBuf::from("/tmp/b.py")]);
        assert_eq!(
            paths_listing_pathful(&some),
            "/tmp/a.py, /tmp/b.py".to_string()
        );
    }
}
