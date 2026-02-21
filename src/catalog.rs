use rmcp::model::Tool;
use serde::Serialize;

/// A tool with its owning server name attached.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogEntry {
    /// Which upstream server this tool belongs to (e.g. "canva", "figma").
    pub server: String,
    /// The tool name as declared by the upstream server.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema for the tool's input parameters (as a JSON value).
    pub input_schema: serde_json::Value,
}

/// Aggregated catalog of tools from all connected MCP servers.
#[derive(Debug, Default)]
pub struct Catalog {
    entries: Vec<CatalogEntry>,
}

impl Catalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register all tools from a given server.
    pub fn add_server_tools(&mut self, server_name: &str, tools: Vec<Tool>) {
        for tool in tools {
            self.entries.push(CatalogEntry {
                server: server_name.to_string(),
                name: tool.name.to_string(),
                description: tool
                    .description
                    .as_deref()
                    .unwrap_or("")
                    .to_string(),
                input_schema: serde_json::to_value(&tool.input_schema).unwrap_or_default(),
            });
        }
    }

    /// Return all entries as a JSON array (for injection into the JS sandbox).
    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.entries).unwrap_or_default()
    }

    /// Get all entries.
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    /// Summarize the catalog for display.
    pub fn summary(&self) -> String {
        let mut servers: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for entry in &self.entries {
            *servers.entry(&entry.server).or_default() += 1;
        }
        let parts: Vec<String> = servers
            .iter()
            .map(|(name, count)| format!("{name}: {count} tools"))
            .collect();
        format!("{} total tools ({})", self.entries.len(), parts.join(", "))
    }
}
