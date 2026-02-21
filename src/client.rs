use std::collections::HashMap;

use anyhow::{Context, Result};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::ConfigureCommandExt;
use rmcp::{RoleClient, ServiceExt};
use tokio::process::Command;
use tracing::info;

use crate::catalog::Catalog;
use crate::config::ServerConfig;

/// A handle to one connected upstream MCP server.
struct UpstreamServer {
    service: RunningService<RoleClient, ()>,
}

/// Manages connections to all upstream MCP servers.
pub struct ClientPool {
    servers: HashMap<String, UpstreamServer>,
}

impl ClientPool {
    /// Connect to all configured servers and build the tool catalog.
    pub async fn connect(
        configs: HashMap<String, ServerConfig>,
    ) -> Result<(Self, Catalog)> {
        let mut servers = HashMap::new();
        let mut catalog = Catalog::new();

        for (name, config) in configs {
            match Self::connect_one(&name, config).await {
                Ok((upstream, tools)) => {
                    info!(server = %name, tool_count = tools.len(), "connected");
                    catalog.add_server_tools(&name, tools);
                    servers.insert(name, upstream);
                }
                Err(e) => {
                    tracing::warn!(server = %name, error = %e, "failed to connect, skipping");
                }
            }
        }

        Ok((Self { servers }, catalog))
    }

    async fn connect_one(
        name: &str,
        config: ServerConfig,
    ) -> Result<(UpstreamServer, Vec<rmcp::model::Tool>)> {
        let service = match config {
            ServerConfig::Http { url, headers: _ } => {
                let transport =
                    rmcp::transport::StreamableHttpClientTransport::from_uri(url);
                ().serve(transport)
                    .await
                    .with_context(|| format!("HTTP connection to {name} failed"))?
            }
            ServerConfig::Sse { url: _, headers: _ } => {
                anyhow::bail!("SSE transport not yet implemented for {name}");
            }
            ServerConfig::Stdio {
                command,
                args,
                env,
            } => {
                let transport = rmcp::transport::TokioChildProcess::new(
                    Command::new(&command).configure(|cmd| {
                        cmd.args(&args);
                        for (k, v) in &env {
                            let resolved = if let Some(var) = v.strip_prefix("env:") {
                                std::env::var(var).unwrap_or_default()
                            } else {
                                v.clone()
                            };
                            cmd.env(k, resolved);
                        }
                    }),
                )?;
                ().serve(transport)
                    .await
                    .with_context(|| format!("stdio connection to {name} failed"))?
            }
        };

        let tools_result = service.list_tools(Default::default()).await?;

        Ok((UpstreamServer { service }, tools_result.tools))
    }

    /// Call a tool on a specific upstream server.
    pub async fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult> {
        let upstream = self
            .servers
            .get(server_name)
            .with_context(|| format!("no server named '{server_name}'"))?;

        let tool_name_owned = tool_name.to_string();

        let result = upstream
            .service
            .call_tool(CallToolRequestParams {
                meta: None,
                name: tool_name_owned.into(),
                arguments: arguments.as_object().cloned(),
                task: None,
            })
            .await
            .with_context(|| format!("tool call {server_name}.{tool_name} failed"))?;

        Ok(result)
    }

    /// Shut down all connections.
    pub async fn shutdown(self) {
        for (name, upstream) in self.servers {
            if let Err(e) = upstream.service.cancel().await {
                tracing::warn!(server = %name, error = %e, "error shutting down");
            }
        }
    }
}
