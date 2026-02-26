use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};

use cmcp_core::config::ServerConfig;

/// A discovered MCP server from an external source.
#[derive(Debug)]
pub struct ImportedServer {
    pub name: String,
    pub config: ServerConfig,
    pub source: ImportSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSource {
    ClaudeCode,
    Codex,
}

impl std::fmt::Display for ImportSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportSource::ClaudeCode => write!(f, "claude"),
            ImportSource::Codex => write!(f, "codex"),
        }
    }
}

/// Scan all known config locations and return discovered servers.
pub fn discover(source_filter: Option<ImportSource>) -> Result<Vec<ImportedServer>> {
    let mut servers = Vec::new();

    if source_filter.is_none() || source_filter == Some(ImportSource::ClaudeCode) {
        servers.extend(discover_claude_code()?);
    }

    if source_filter.is_none() || source_filter == Some(ImportSource::Codex) {
        servers.extend(discover_codex()?);
    }

    Ok(servers)
}

// ── Claude ───────────────────────────────────────────────────────────

fn discover_claude_code() -> Result<Vec<ImportedServer>> {
    let mut servers = Vec::new();
    let home = home_dir()?;

    // User-scoped: ~/.claude.json
    let user_config = home.join(".claude.json");
    if user_config.exists() {
        servers.extend(parse_claude_code_json(&user_config)?);
    }

    // Project-scoped: .mcp.json (current directory)
    let project_config = PathBuf::from(".mcp.json");
    if project_config.exists() {
        servers.extend(parse_claude_code_json(&project_config)?);
    }

    Ok(servers)
}

fn parse_claude_code_json(path: &PathBuf) -> Result<Vec<ImportedServer>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let root: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;

    let Some(mcp_servers) = root.get("mcpServers").and_then(|v| v.as_object()) else {
        return Ok(Vec::new());
    };

    let mut servers = Vec::new();

    for (name, value) in mcp_servers {
        match parse_claude_code_server(name, value) {
            Ok(Some(server)) => servers.push(server),
            Ok(None) => {} // unsupported transport, skip
            Err(e) => {
                eprintln!("  warning: skipping {name}: {e}");
            }
        }
    }

    Ok(servers)
}

fn parse_claude_code_server(
    name: &str,
    value: &serde_json::Value,
) -> Result<Option<ImportedServer>> {
    let obj = value.as_object().context("server config is not an object")?;

    let transport = obj
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("stdio");

    let config = match transport {
        "stdio" => {
            let command = obj
                .get("command")
                .and_then(|v| v.as_str())
                .context("missing command")?
                .to_string();

            let args = obj
                .get("args")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            let env = parse_json_string_map(obj.get("env"));

            ServerConfig::Stdio { command, args, env }
        }
        "http" => {
            let url = obj
                .get("url")
                .and_then(|v| v.as_str())
                .context("missing url")?
                .to_string();

            let headers = parse_json_string_map(obj.get("headers"));

            // Extract auth from Authorization header if present.
            let (auth, headers) = extract_auth_header(headers);

            ServerConfig::Http { url, auth, headers }
        }
        "sse" => {
            let url = obj
                .get("url")
                .and_then(|v| v.as_str())
                .context("missing url")?
                .to_string();

            let headers = parse_json_string_map(obj.get("headers"));
            let (auth, headers) = extract_auth_header(headers);

            ServerConfig::Sse { url, auth, headers }
        }
        // Skip internal types: ws, sse-ide, ws-ide, sdk, claudeai-proxy
        _ => return Ok(None),
    };

    Ok(Some(ImportedServer {
        name: name.to_string(),
        config,
        source: ImportSource::ClaudeCode,
    }))
}

// ── Codex ────────────────────────────────────────────────────────────

fn discover_codex() -> Result<Vec<ImportedServer>> {
    let mut servers = Vec::new();
    let home = home_dir()?;

    // User-scoped: ~/.codex/config.toml
    let user_config = home.join(".codex").join("config.toml");
    if user_config.exists() {
        servers.extend(parse_codex_toml(&user_config)?);
    }

    // Project-scoped: .codex/config.toml
    let project_config = PathBuf::from(".codex").join("config.toml");
    if project_config.exists() {
        servers.extend(parse_codex_toml(&project_config)?);
    }

    Ok(servers)
}

fn parse_codex_toml(path: &PathBuf) -> Result<Vec<ImportedServer>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let root: toml::Value = content
        .parse()
        .with_context(|| format!("failed to parse {}", path.display()))?;

    let Some(mcp_servers) = root.get("mcp_servers").and_then(|v| v.as_table()) else {
        return Ok(Vec::new());
    };

    let mut servers = Vec::new();

    for (name, value) in mcp_servers {
        match parse_codex_server(name, value) {
            Ok(Some(server)) => servers.push(server),
            Ok(None) => {} // disabled, skip
            Err(e) => {
                eprintln!("  warning: skipping {name}: {e}");
            }
        }
    }

    Ok(servers)
}

fn parse_codex_server(name: &str, value: &toml::Value) -> Result<Option<ImportedServer>> {
    let table = value.as_table().context("server config is not a table")?;

    // Skip disabled servers.
    if let Some(enabled) = table.get("enabled").and_then(|v| v.as_bool()) {
        if !enabled {
            return Ok(None);
        }
    }

    let has_url = table.get("url").is_some();
    let has_command = table.get("command").is_some();

    let config = if has_url {
        // Streamable HTTP
        let url = table
            .get("url")
            .and_then(|v| v.as_str())
            .context("missing url")?
            .to_string();

        // Auth: bearer_token_env_var -> "env:VAR", bearer_token -> literal
        let auth = if let Some(env_var) = table
            .get("bearer_token_env_var")
            .and_then(|v| v.as_str())
        {
            Some(format!("env:{env_var}"))
        } else {
            table
                .get("bearer_token")
                .and_then(|v| v.as_str())
                .map(String::from)
        };

        // http_headers (static) + env_http_headers (env var references)
        let mut headers = parse_toml_string_map(table.get("http_headers"));
        if let Some(env_headers) = table.get("env_http_headers").and_then(|v| v.as_table()) {
            for (k, v) in env_headers {
                if let Some(env_var) = v.as_str() {
                    headers.insert(k.clone(), format!("env:{env_var}"));
                }
            }
        }

        ServerConfig::Http { url, auth, headers }
    } else if has_command {
        // Stdio
        let command = table
            .get("command")
            .and_then(|v| v.as_str())
            .context("missing command")?
            .to_string();

        let args = table
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let mut env = parse_toml_string_map(table.get("env"));

        // env_vars: forward named vars from parent environment
        if let Some(env_vars) = table.get("env_vars").and_then(|v| v.as_array()) {
            for var in env_vars {
                if let Some(var_name) = var.as_str() {
                    env.insert(var_name.to_string(), format!("env:{var_name}"));
                }
            }
        }

        ServerConfig::Stdio { command, args, env }
    } else {
        anyhow::bail!("server has neither 'url' nor 'command'");
    };

    Ok(Some(ImportedServer {
        name: name.to_string(),
        config,
        source: ImportSource::Codex,
    }))
}

// ── Helpers ──────────────────────────────────────────────────────────

fn parse_json_string_map(value: Option<&serde_json::Value>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(obj) = value.and_then(|v| v.as_object()) {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                map.insert(k.clone(), s.to_string());
            }
        }
    }
    map
}

fn parse_toml_string_map(value: Option<&toml::Value>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(table) = value.and_then(|v| v.as_table()) {
        for (k, v) in table {
            if let Some(s) = v.as_str() {
                map.insert(k.clone(), s.to_string());
            }
        }
    }
    map
}

/// If the headers contain an "Authorization: Bearer <token>" entry,
/// extract it as an auth value and return the remaining headers.
fn extract_auth_header(mut headers: HashMap<String, String>) -> (Option<String>, HashMap<String, String>) {
    let auth = headers
        .remove("Authorization")
        .or_else(|| headers.remove("authorization"))
        .and_then(|v| {
            if let Some(token) = v.strip_prefix("Bearer ") {
                Some(token.to_string())
            } else if let Some(token) = v.strip_prefix("bearer ") {
                Some(token.to_string())
            } else {
                // Non-bearer auth — put it back as a header.
                headers.insert("Authorization".to_string(), v);
                None
            }
        });
    (auth, headers)
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME not set")
}
