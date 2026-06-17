//! `cat198x mcp` — a Model Context Protocol server over stdio.
//!
//! It exposes the shared operation surface (`crate::ops`) as MCP tools so an
//! agent can drive Cat198x headlessly — the same operations the CLI formats and
//! the Tauri UI will call. This is the MCP adapter of the one-surface/three-
//! adapters design in `decisions/agent-native-surface-and-ui.md`.
//!
//! stdout is the JSON-RPC transport, so nothing here prints to it: the operation
//! functions are silent by construction, logging is routed to stderr by `main`,
//! and each tool returns its data as JSON text content.
//!
//! This first cut serves the read-only operations (status, the saved
//! plan-as-diff, collection/source listings). Each tool opens its own short-
//! lived database connection inside a blocking task — rusqlite connections are
//! not `Send`, and MCP calls are infrequent, so a fresh connection per call is
//! simplest and safe.

use std::path::PathBuf;

use anyhow::Result;
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};

use crate::db::Database;
use crate::db::dats::MergeMode;

use super::get_data_dir;

/// Arguments for the `status` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct StatusArgs {
    /// Restrict to a single collection by exact name; omit for every collection.
    #[serde(default)]
    collection: Option<String>,
    /// Merge mode for completeness: "non-merged" (default), "split", or "merged".
    #[serde(default)]
    merge_mode: Option<String>,
}

#[derive(Clone)]
struct Cat198xServer {
    db_path: PathBuf,
    data_dir: PathBuf,
    // Built by the `#[tool_router]` macro and consumed by `#[tool_handler]`; the
    // compiler can't see the macro-generated read, so it reads as dead.
    #[allow(dead_code)]
    tool_router: ToolRouter<Cat198xServer>,
}

impl Cat198xServer {
    fn new(db_path: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            db_path,
            data_dir,
            tool_router: Self::tool_router(),
        }
    }
}

/// Map a merge-mode string to the enum, defaulting to non-merged.
fn parse_merge_mode(s: Option<&str>) -> MergeMode {
    match s {
        Some("split") => MergeMode::Split,
        Some("merged") => MergeMode::Merged,
        _ => MergeMode::NonMerged,
    }
}

/// Wrap an `anyhow` failure as an MCP internal error.
fn mcp_err(e: anyhow::Error) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

/// Serialize a value to pretty JSON and wrap it as a tool's text result.
fn json_result<T: serde::Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
}

#[tool_router]
impl Cat198xServer {
    #[tool(
        description = "Collection completeness against the active DATs: games, ROMs required, have, and missing per collection. Optionally filter to one collection; choose a merge mode (non-merged/split/merged)."
    )]
    async fn status(
        &self,
        Parameters(args): Parameters<StatusArgs>,
    ) -> Result<CallToolResult, McpError> {
        let db_path = self.db_path.clone();
        let statuses = tokio::task::spawn_blocking(move || -> Result<_> {
            let db = Database::open(&db_path)?;
            let mode = parse_merge_mode(args.merge_mode.as_deref());
            crate::ops::collection_status(db.conn(), args.collection.as_deref(), mode)
        })
        .await
        .map_err(|e| McpError::internal_error(e.to_string(), None))?
        .map_err(mcp_err)?;
        json_result(&statuses)
    }

    #[tool(
        description = "List every registered collection and whether it has an active DAT version."
    )]
    async fn list_collections(&self) -> Result<CallToolResult, McpError> {
        let db_path = self.db_path.clone();
        let collections = tokio::task::spawn_blocking(move || -> Result<_> {
            let db = Database::open(&db_path)?;
            crate::ops::list_collections(db.conn())
        })
        .await
        .map_err(|e| McpError::internal_error(e.to_string(), None))?
        .map_err(mcp_err)?;
        json_result(&collections)
    }

    #[tool(description = "List every registered source directory and when it was last scanned.")]
    async fn list_sources(&self) -> Result<CallToolResult, McpError> {
        let db_path = self.db_path.clone();
        let sources = tokio::task::spawn_blocking(move || -> Result<_> {
            let db = Database::open(&db_path)?;
            crate::ops::list_sources(db.conn())
        })
        .await
        .map_err(|e| McpError::internal_error(e.to_string(), None))?
        .map_err(mcp_err)?;
        json_result(&sources)
    }

    #[tool(
        description = "The most recent saved plan — the reorganisation as a diff of operations (copy/move/repack/delete/quarantine) with a summary. Returns null when no plan has been generated."
    )]
    async fn get_plan(&self) -> Result<CallToolResult, McpError> {
        let data_dir = self.data_dir.clone();
        let plan = tokio::task::spawn_blocking(move || crate::ops::latest_plan(&data_dir))
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .map_err(mcp_err)?;
        json_result(&plan)
    }
}

#[tool_handler]
impl ServerHandler for Cat198xServer {
    fn get_info(&self) -> ServerInfo {
        // Identify as cat198x, not the rmcp crate `from_build_env` would pick up.
        // `Implementation` is non-exhaustive, so set the public fields on a base.
        let mut implementation = Implementation::from_build_env();
        implementation.name = "cat198x".to_string();
        implementation.version = env!("CARGO_PKG_VERSION").to_string();

        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(implementation)
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(
                "Cat198x ROM-catalogue tools (read-only). status: collection \
                 completeness; list_collections / list_sources: the catalogue's \
                 collections and source directories; get_plan: the latest saved \
                 reorganisation plan as a diff. Mutating operations are not yet \
                 exposed."
                    .to_string(),
            )
    }
}

/// Run the MCP server over stdio until the client disconnects.
///
/// `main` has already routed logging to stderr for this subcommand, leaving
/// stdout clear for the JSON-RPC stream.
pub fn run(data_dir: Option<PathBuf>) -> Result<()> {
    let dir = get_data_dir(data_dir)?;
    let db_path = dir.join("db.sqlite");
    if !db_path.exists() {
        anyhow::bail!(
            "Cat198x not initialized. Run 'cat198x init' first.\n\
             Expected database at: {}",
            db_path.display()
        );
    }

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        let service = Cat198xServer::new(db_path, dir).serve(stdio()).await?;
        service.waiting().await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::files;

    #[test]
    fn parse_merge_mode_defaults_to_non_merged() {
        assert_eq!(parse_merge_mode(Some("split")), MergeMode::Split);
        assert_eq!(parse_merge_mode(Some("merged")), MergeMode::Merged);
        assert_eq!(parse_merge_mode(Some("non-merged")), MergeMode::NonMerged);
        assert_eq!(parse_merge_mode(None), MergeMode::NonMerged);
        assert_eq!(parse_merge_mode(Some("nonsense")), MergeMode::NonMerged);
    }

    /// Drive a tool method end to end against a temp database: the server opens
    /// its own connection, runs the silent operation, and returns JSON content.
    #[tokio::test]
    async fn list_sources_tool_returns_the_catalogue_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("db.sqlite");
        {
            let db = Database::open(&db_path).unwrap();
            files::add_source(db.conn(), "/lib/ROMs", false).unwrap();
        }

        let server = Cat198xServer::new(db_path, tmp.path().to_path_buf());
        let result = server.list_sources().await.expect("tool call succeeds");

        // Serialize the result and confirm it carries the source, not an error.
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("/lib/ROMs"), "source path present in content");
        assert!(
            !json.contains("\"isError\":true"),
            "tool did not report an error"
        );
    }

    #[tokio::test]
    async fn get_plan_tool_returns_null_without_a_saved_plan() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("db.sqlite");
        Database::open(&db_path).unwrap();

        let server = Cat198xServer::new(db_path, tmp.path().to_path_buf());
        let result = server.get_plan().await.expect("tool call succeeds");
        let json = serde_json::to_string(&result).unwrap();
        // latest_plan is None -> serialized as JSON null in the text content.
        assert!(json.contains("null"));
    }
}
