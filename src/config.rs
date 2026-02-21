use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Top-level configuration.
#[derive(Debug, Deserialize, Serialize, Default)]
pub struct Config {
    #[serde(default)]
    pub servers: HashMap<String, ServerConfig>,
}

/// Configuration for a single upstream MCP server.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "transport")]
pub enum ServerConfig {
    #[serde(rename = "http")]
    Http {
        url: String,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        headers: HashMap<String, String>,
    },

    #[serde(rename = "sse")]
    Sse {
        url: String,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        headers: HashMap<String, String>,
    },

    #[serde(rename = "stdio")]
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        env: HashMap<String, String>,
    },
}

impl Config {
    /// Load config, falling back to defaults if the file doesn't exist.
    pub fn load(path: Option<&PathBuf>) -> Result<Self> {
        let path = match path {
            Some(p) => p.clone(),
            None => default_config_path()?,
        };

        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read config from {}", path.display()))?;

        toml::from_str(&content)
            .with_context(|| format!("failed to parse config from {}", path.display()))
    }

    /// Save config to file, creating parent dirs as needed.
    pub fn save(&self, path: Option<&PathBuf>) -> Result<()> {
        let path = match path {
            Some(p) => p.clone(),
            None => default_config_path()?,
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let content = toml::to_string_pretty(self)
            .context("failed to serialize config")?;

        std::fs::write(&path, content)
            .with_context(|| format!("failed to write config to {}", path.display()))
    }

    pub fn add_server(&mut self, name: String, config: ServerConfig) {
        self.servers.insert(name, config);
    }

    pub fn remove_server(&mut self, name: &str) -> bool {
        self.servers.remove(name).is_some()
    }
}

pub fn default_config_path() -> Result<PathBuf> {
    let config_dir = dirs_config_dir().context("could not determine config directory")?;
    Ok(config_dir.join("code-mode-mcp").join("config.toml"))
}

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
