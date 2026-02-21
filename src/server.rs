use std::sync::Arc;

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::catalog::Catalog;
use crate::client::ClientPool;
use crate::sandbox::Sandbox;

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchRequest {
    #[schemars(description = "TypeScript code to filter/explore the tools catalog. A typed `tools` array is available with fields: { server, name, description, input_schema }. Must return a value. Example: return tools.filter(t => t.description.toLowerCase().includes(\"design\"))")]
    code: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ExecuteRequest {
    #[schemars(description = "TypeScript code to execute. Each connected server is a typed global object where every tool is an async function. Type declarations are auto-generated from tool schemas. Example: const result = await canva.create_design({ type: \"poster\" }); return result;")]
    code: String,
}

/// The code-mode MCP server that exposes `search` and `execute` tools.
#[derive(Clone)]
pub struct CodeModeServer {
    sandbox: Arc<Mutex<Sandbox>>,
    catalog: Arc<Catalog>,
    tool_router: ToolRouter<Self>,
}

impl CodeModeServer {
    pub async fn new(pool: ClientPool, catalog: Catalog) -> anyhow::Result<Self> {
        let catalog = Arc::new(catalog);
        let pool = Arc::new(Mutex::new(pool));
        let sandbox = Sandbox::new(pool, catalog.clone()).await?;

        Ok(Self {
            sandbox: Arc::new(Mutex::new(sandbox)),
            catalog,
            tool_router: Self::tool_router(),
        })
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
        let sandbox = self.sandbox.lock().await;
        match sandbox.search(&req.code).await {
            Ok(result) => {
                let text = serde_json::to_string_pretty(&result).unwrap_or_default();
                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "search error: {e}"
            ))])),
        }
    }

    #[tool(
        name = "execute",
        description = "Execute TypeScript code that calls tools across all connected MCP servers. Each server is a typed global object (e.g. `canva`, `figma`) where every tool is an async function with typed parameters: `await server.tool_name({ param: value })`."
    )]
    async fn execute(
        &self,
        Parameters(req): Parameters<ExecuteRequest>,
    ) -> Result<CallToolResult, McpError> {
        let sandbox = self.sandbox.lock().await;
        match sandbox.execute(&req.code).await {
            Ok(result) => {
                let text = serde_json::to_string_pretty(&result).unwrap_or_default();
                Ok(CallToolResult::success(vec![Content::text(text)]))
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
        let summary = self.catalog.summary();
        ServerInfo {
            instructions: Some(format!(
                "Code Mode MCP Proxy — {summary}.\n\n\
                 Use `search` to discover available tools by writing TypeScript filter code.\n\
                 Use `execute` to call tools across servers by writing TypeScript code.\n\n\
                 Each connected server is a typed object in `execute` with auto-generated type declarations from tool schemas.\n\
                 Example: `await canva.create_design({{ type: \"poster\" }})`"
            )),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}
