use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Scope for where a config lives — mirrors Claude's scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// User-global: ~/.config/code-mode-mcp/config.toml
    User,
    /// Per-project: .cmcp.toml in project root
    Project,
    /// Machine-local (same as user for now)
    Local,
}

impl Scope {
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "user" | "global" => Ok(Self::User),
            "project" => Ok(Self::Project),
            "local" => Ok(Self::Local),
            other => anyhow::bail!("unknown scope \"{other}\". Use: local, user, or project"),
        }
    }

    /// Resolve to a config file path.
    pub fn config_path(&self) -> Result<PathBuf> {
        match self {
            Self::User | Self::Local => default_config_path(),
            Self::Project => Ok(project_config_path()),
        }
    }
}

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
        /// Bearer token (without "Bearer " prefix).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth: Option<String>,
        /// Custom HTTP headers sent with every request.
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        headers: HashMap<String, String>,
    },

    #[serde(rename = "sse")]
    Sse {
        url: String,
        /// Bearer token (without "Bearer " prefix).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth: Option<String>,
        /// Custom HTTP headers.
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
    /// Load config from a specific path, falling back to defaults if the file doesn't exist.
    pub fn load_from(path: &PathBuf) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config from {}", path.display()))?;

        toml::from_str(&content)
            .with_context(|| format!("failed to parse config from {}", path.display()))
    }

    /// Load config, falling back to defaults if the file doesn't exist.
    pub fn load(path: Option<&PathBuf>) -> Result<Self> {
        let path = match path {
            Some(p) => p.clone(),
            None => default_config_path()?,
        };
        Self::load_from(&path)
    }

    /// Load merged config: user config as base, then overlay project and explicit configs.
    /// Later configs override earlier ones with the same server name.
    /// Priority (lowest to highest): user → project (.cmcp.toml) → explicit_path
    pub fn load_merged(explicit_path: Option<&PathBuf>) -> Result<Self> {
        // Always start with user config as the base.
        let user_path = default_config_path()?;
        let mut merged = Self::load_from(&user_path)?;

        // Overlay project config (.cmcp.toml) if it exists.
        let project_path = project_config_path();
        if project_path.exists() {
            let project = Self::load_from(&project_path)?;
            for (name, config) in project.servers {
                merged.servers.insert(name, config);
            }
        }

        // Overlay explicit config (e.g. .cas/proxy.toml) if provided.
        if let Some(p) = explicit_path {
            let explicit = Self::load_from(p)?;
            for (name, config) in explicit.servers {
                merged.servers.insert(name, config);
            }
        }

        Ok(merged)
    }

    /// Save config to a specific path, creating parent dirs as needed.
    pub fn save_to(&self, path: &PathBuf) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let content = toml::to_string_pretty(self)
            .context("failed to serialize config")?;

        std::fs::write(path, content)
            .with_context(|| format!("failed to write config to {}", path.display()))
    }

    /// Save config to file, creating parent dirs as needed.
    pub fn save(&self, path: Option<&PathBuf>) -> Result<()> {
        let path = match path {
            Some(p) => p.clone(),
            None => default_config_path()?,
        };
        self.save_to(&path)
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

/// Project-scoped config: .cmcp.toml in the current directory.
pub fn project_config_path() -> PathBuf {
    PathBuf::from(".cmcp.toml")
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
