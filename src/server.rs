use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::info;

use cmcp_core::config;
use cmcp_core::{ProxyEngine, truncate_response};

/// Default max response length in characters (~10k tokens).
const DEFAULT_MAX_LENGTH: usize = 40_000;

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchRequest {
    #[schemars(description = "TypeScript code to filter/explore the tools catalog. A typed `tools` array is available with fields: { server, name, description, input_schema }. Must return a value. Example: return tools.filter(t => t.description.toLowerCase().includes(\"design\"))")]
    code: String,
    #[schemars(description = "Max response length in characters. Default: 40000. Use your code to extract only what you need rather than increasing this.")]
    #[serde(default)]
    max_length: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ExecuteRequest {
    #[schemars(description = "TypeScript code to execute. Each connected server is a typed global object where every tool is an async function. Type declarations are auto-generated from tool schemas. Chain calls sequentially: await chrome_devtools.navigate_page({ url: \"https://example.com\" }); const screenshot = await chrome_devtools.take_screenshot({ format: \"png\" }); return screenshot; Or run calls in parallel with Promise.all: const [issues, designs] = await Promise.all([github.list_issues({ repo: \"myorg/app\" }), canva.list_designs({})]);")]
    code: String,
    #[schemars(description = "Max response length in characters. Default: 40000. Use your code to extract only what you need rather than increasing this.")]
    #[serde(default)]
    max_length: Option<usize>,
}

/// Hot-reload state: tracks config file mtimes.
struct HotReloadState {
    user_mtime: Option<SystemTime>,
    project_mtime: Option<SystemTime>,
}

/// The code-mode MCP server that exposes `search` and `execute` tools.
#[derive(Clone)]
pub struct CodeModeServer {
    engine: Arc<ProxyEngine>,
    reload_state: Arc<Mutex<HotReloadState>>,
    config_path: Option<PathBuf>,
    tool_router: ToolRouter<Self>,
}

/// Get the modification time of a file, or None if it doesn't exist.
fn file_mtime(path: &PathBuf) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

impl CodeModeServer {
    pub async fn new(
        servers: std::collections::HashMap<String, config::ServerConfig>,
        config_path: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let engine = ProxyEngine::from_configs(servers).await?;

        // Snapshot current config file mtimes.
        let user_mtime = config::default_config_path()
            .ok()
            .and_then(|p| file_mtime(&p));
        let project_mtime = file_mtime(&config::project_config_path());

        Ok(Self {
            engine: Arc::new(engine),
            reload_state: Arc::new(Mutex::new(HotReloadState {
                user_mtime,
                project_mtime,
            })),
            config_path,
            tool_router: Self::tool_router(),
        })
    }

    /// Check if config files have changed and reload if needed.
    async fn maybe_reload(&self) {
        let needs_reload = {
            let state = self.reload_state.lock().await;

            let current_user_mtime = config::default_config_path()
                .ok()
                .and_then(|p| file_mtime(&p));
            let current_project_mtime = file_mtime(&config::project_config_path());

            current_user_mtime != state.user_mtime
                || current_project_mtime != state.project_mtime
        };

        if !needs_reload {
            return;
        }

        info!("config change detected, reloading servers...");

        let cfg = match config::Config::load_merged(self.config_path.as_ref()) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!(error = %e, "failed to reload config, keeping current state");
                return;
            }
        };

        if let Err(e) = self.engine.reload(cfg.servers).await {
            tracing::warn!(error = %e, "failed to reload proxy engine, keeping current state");
            return;
        }

        info!("{}", self.engine.summary().await);

        let user_mtime = config::default_config_path()
            .ok()
            .and_then(|p| file_mtime(&p));
        let project_mtime = file_mtime(&config::project_config_path());

        let mut state = self.reload_state.lock().await;
        state.user_mtime = user_mtime;
        state.project_mtime = project_mtime;

        info!("hot-reload complete");
    }
}

#[tool_router]
impl CodeModeServer {
    #[tool(
        name = "search",
        description = "Search across all tools from all connected MCP servers. Write TypeScript code to filter the tool catalog. A typed `tools` array is available with { server, name, description, input_schema } fields."
    )]
    async fn search(
        &self,
        Parameters(req): Parameters<SearchRequest>,
    ) -> Result<CallToolResult, McpError> {
        self.maybe_reload().await;

        match self.engine.search(&req.code, req.max_length).await {
            Ok(result) => {
                let text = serde_json::to_string_pretty(&result).unwrap_or_default();
                Ok(CallToolResult::success(vec![Content::text(
                    truncate_response(text, req.max_length.unwrap_or(DEFAULT_MAX_LENGTH)),
                )]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "search error: {e}"
            ))])),
        }
    }

    #[tool(
        name = "execute",
        description = "Execute TypeScript code that calls tools across all connected MCP servers. Each server is a typed global object (e.g. `canva`, `figma`) where every tool is an async function with typed parameters: `await server.tool_name({ param: value })`. Chain calls sequentially or run them in parallel with Promise.all across different servers."
    )]
    async fn execute(
        &self,
        Parameters(req): Parameters<ExecuteRequest>,
    ) -> Result<CallToolResult, McpError> {
        self.maybe_reload().await;

        match self.engine.execute(&req.code, req.max_length).await {
            Ok(result) => {
                let mut content = vec![Content::text(result.text)];
                for img in result.images {
                    content.push(Content::image(img.data, img.mime_type));
                }
                Ok(CallToolResult::success(content))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "execute error: {e}"
            ))])),
        }
    }
}

#[tool_handler]
impl ServerHandler for CodeModeServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Code Mode MCP Proxy.\n\n\
                 Use `search` to discover available tools by writing TypeScript filter code.\n\
                 Use `execute` to call tools across servers by writing TypeScript code.\n\n\
                 Each connected server is a typed object in `execute` with auto-generated type declarations from tool schemas.\n\
                 Example: `await canva.create_design({ type: \"poster\" })`\n\n\
                 Hot-reload: add or remove servers with `cmcp add`/`cmcp remove` — changes are picked up on the next call."
                    .to_string(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}
