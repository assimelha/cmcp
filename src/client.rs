use std::collections::HashMap;

use anyhow::{Context, Result};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::ConfigureCommandExt;
use rmcp::{RoleClient, ServiceExt};
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::info;

use crate::catalog::Catalog;
use crate::config::ServerConfig;

/// A handle to one connected upstream MCP server with its config for reconnection.
struct UpstreamServer {
    service: RunningService<RoleClient, ()>,
    config: ServerConfig,
}

/// Manages connections to all upstream MCP servers.
pub struct ClientPool {
    servers: HashMap<String, Mutex<UpstreamServer>>,
}

impl ClientPool {
    /// Connect to all configured servers and build the tool catalog.
    pub async fn connect(
        configs: HashMap<String, ServerConfig>,
    ) -> Result<(Self, Catalog)> {
        let mut servers = HashMap::new();
        let mut catalog = Catalog::new();

        for (name, config) in configs {
            match Self::connect_one(&name, &config).await {
                Ok((service, tools)) => {
                    info!(server = %name, tool_count = tools.len(), "connected");
                    catalog.add_server_tools(&name, tools);
                    servers.insert(
                        name,
                        Mutex::new(UpstreamServer { service, config }),
                    );
                }
                Err(e) => {
                    tracing::warn!(server = %name, error = %e, "failed to connect, skipping");
                }
            }
        }

        Ok((Self { servers }, catalog))
    }

    /// Build the transport config for HTTP/SSE servers.
    fn build_http_config(
        url: &str,
        auth: &Option<String>,
        headers: &HashMap<String, String>,
    ) -> StreamableHttpClientTransportConfig {
        let mut config = StreamableHttpClientTransportConfig::with_uri(url);

        // Auth header (bearer token)
        if let Some(token) = auth {
            let resolved = resolve_env(token);
            config = config.auth_header(resolved);
        }

        // Custom headers
        if !headers.is_empty() {
            let mut header_map = HashMap::new();
            for (k, v) in headers {
                let resolved_v = resolve_env(v);
                if let (Ok(name), Ok(value)) = (
                    http::HeaderName::try_from(k.as_str()),
                    http::HeaderValue::try_from(resolved_v.as_str()),
                ) {
                    header_map.insert(name, value);
                }
            }
            config = config.custom_headers(header_map);
        }

        config
    }

    async fn connect_one(
        name: &str,
        config: &ServerConfig,
    ) -> Result<(RunningService<RoleClient, ()>, Vec<rmcp::model::Tool>)> {
        let service = match config {
            ServerConfig::Http { url, auth, headers } | ServerConfig::Sse { url, auth, headers } => {
                let transport_config = Self::build_http_config(url, auth, headers);
                let transport =
                    rmcp::transport::StreamableHttpClientTransport::from_config(transport_config);
                ().serve(transport)
                    .await
                    .with_context(|| format!("connection to {name} failed"))?
            }
            ServerConfig::Stdio {
                command,
                args,
                env,
            } => {
                let transport = rmcp::transport::TokioChildProcess::new(
                    Command::new(command).configure(|cmd| {
                        cmd.args(args);
                        for (k, v) in env {
                            cmd.env(k, resolve_env(v));
                        }
                    }),
                )?;
                ().serve(transport)
                    .await
                    .with_context(|| format!("stdio connection to {name} failed"))?
            }
        };

        let tools_result = service.list_tools(Default::default()).await?;
        Ok((service, tools_result.tools))
    }

    /// Call a tool on a specific upstream server.
    /// If the connection is dead, attempts one reconnect.
    pub async fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult> {
        let upstream_mutex = self
            .servers
            .get(server_name)
            .with_context(|| format!("no server named '{server_name}'"))?;

        let mut upstream = upstream_mutex.lock().await;

        let tool_name_owned = tool_name.to_string();
        let make_params = |name: String| CallToolRequestParams {
            meta: None,
            name: name.into(),
            arguments: arguments.as_object().cloned(),
            task: None,
        };

        // First attempt
        let result = upstream
            .service
            .call_tool(make_params(tool_name_owned.clone()))
            .await;

        match result {
            Ok(r) => return Ok(r),
            Err(first_err) => {
                // Try to reconnect once
                tracing::warn!(
                    server = %server_name,
                    error = %first_err,
                    "tool call failed, attempting reconnect"
                );

                match Self::connect_one(server_name, &upstream.config).await {
                    Ok((new_service, _tools)) => {
                        upstream.service = new_service;

                        // Retry the tool call
                        let retry = upstream
                            .service
                            .call_tool(make_params(tool_name_owned))
                            .await
                            .with_context(|| {
                                format!("tool call {server_name}.{tool_name} failed after reconnect")
                            })?;

                        Ok(retry)
                    }
                    Err(reconnect_err) => {
                        anyhow::bail!(
                            "tool call {server_name}.{tool_name} failed: {first_err}; reconnect also failed: {reconnect_err}"
                        );
                    }
                }
            }
        }
    }

}

/// Resolve "env:VAR_NAME" references to environment variable values.
fn resolve_env(value: &str) -> String {
    if let Some(var) = value.strip_prefix("env:") {
        std::env::var(var).unwrap_or_default()
    } else {
        value.to_string()
    }
}
