use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Top-level configuration.
#[derive(Debug, Deserialize)]
pub struct Config {
    pub servers: HashMap<String, ServerConfig>,
}

/// Configuration for a single upstream MCP server.
#[derive(Debug, Deserialize)]
#[serde(tag = "transport")]
pub enum ServerConfig {
    /// Streamable HTTP transport (the modern default).
    #[serde(rename = "http")]
    Http {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },

    /// Legacy SSE transport.
    #[serde(rename = "sse")]
    Sse {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },

    /// Stdio transport — spawns a subprocess.
    #[serde(rename = "stdio")]
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
}

impl Config {
    /// Load config from a TOML file path, falling back to the default location.
    pub fn load(path: Option<&PathBuf>) -> Result<Self> {
        let path = match path {
            Some(p) => p.clone(),
            None => default_config_path()?,
        };

        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read config from {}", path.display()))?;

        toml::from_str(&content)
            .with_context(|| format!("failed to parse config from {}", path.display()))
    }
}

fn default_config_path() -> Result<PathBuf> {
    let config_dir = dirs_config_dir().context("could not determine config directory")?;
    Ok(config_dir.join("code-mode-mcp").join("config.toml"))
}

/// Platform-appropriate config dir (~/.config on Linux/macOS).
fn dirs_config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(PathBuf::from)
    }
}
