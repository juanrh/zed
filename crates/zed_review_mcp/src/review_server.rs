use crate::review_store::{RecordOutcome, ReviewStore};
use anyhow::{Error, Result};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities, ServerConfig,
};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use std::path::PathBuf;
use std::sync::Arc;

pub const SERVER_NAME: &str = "zed-review-mcp";

pub struct ReviewServer {
    review_store: Arc<ReviewStore>,
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

impl ReviewServer {
    pub fn new(review_store: Arc<ReviewStore>) -> Self {
        Self {
            review_store,
            tool_router: Self::tool_router(),
        }
    }

    pub fn review_store(&self) -> &ReviewStore {
        &self.review_store
    }
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
