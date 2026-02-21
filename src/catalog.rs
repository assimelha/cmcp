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

    /// Generate TypeScript type declarations for all servers and their tools.
    ///
    /// Produces `declare const <server>: { ... }` blocks so the agent
    /// gets autocomplete-style hints when writing execute() code.
    pub fn type_declarations(&self) -> String {
        let mut servers: std::collections::BTreeMap<&str, Vec<&CatalogEntry>> =
            std::collections::BTreeMap::new();
        for entry in &self.entries {
            servers.entry(&entry.server).or_default().push(entry);
        }

        let mut out = String::new();

        // tools array type
        out.push_str("declare const tools: Array<{ server: string; name: string; description: string; input_schema: any }>;\n\n");

        for (server, tools) in &servers {
            out.push_str(&format!("declare const {server}: {{\n"));
            for tool in tools {
                let params_type = schema_to_ts_params(&tool.input_schema);
                let desc = tool.description.replace('\n', " ");
                if !desc.is_empty() {
                    out.push_str(&format!("  /** {desc} */\n"));
                }
                out.push_str(&format!(
                    "  {name}(params: {{ {params_type} }}): Promise<any>;\n",
                    name = tool.name,
                ));
            }
            out.push_str("};\n\n");
        }

        out
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

/// Convert a JSON Schema `input_schema` to a TypeScript-style parameter string.
///
/// Given `{ "type": "object", "properties": { "title": { "type": "string" }, "width": { "type": "number" } }, "required": ["title"] }`,
/// produces `title: string; width?: number`.
fn schema_to_ts_params(schema: &serde_json::Value) -> String {
    let Some(properties) = schema.get("properties").and_then(|v| v.as_object()) else {
        return String::new();
    };

    let required: Vec<&str> = schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let mut params = Vec::new();
    for (name, prop) in properties {
        let ts_type = json_type_to_ts(prop);
        let optional = if required.contains(&name.as_str()) {
            ""
        } else {
            "?"
        };
        params.push(format!("{name}{optional}: {ts_type}"));
    }

    params.join("; ")
}

/// Map a JSON Schema type to a TypeScript type string.
fn json_type_to_ts(schema: &serde_json::Value) -> String {
    // Handle enum values
    if let Some(enum_vals) = schema.get("enum").and_then(|v| v.as_array()) {
        let literals: Vec<String> = enum_vals
            .iter()
            .map(|v| match v {
                serde_json::Value::String(s) => format!("\"{s}\""),
                other => other.to_string(),
            })
            .collect();
        return literals.join(" | ");
    }

    let type_str = schema
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("any");

    match type_str {
        "string" => "string".to_string(),
        "number" | "integer" => "number".to_string(),
        "boolean" => "boolean".to_string(),
        "null" => "null".to_string(),
        "array" => {
            if let Some(items) = schema.get("items") {
                format!("{}[]", json_type_to_ts(items))
            } else {
                "any[]".to_string()
            }
        }
        "object" => {
            if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
                if props.is_empty() {
                    "Record<string, any>".to_string()
                } else {
                    let inner = schema_to_ts_params(schema);
                    format!("{{ {inner} }}")
                }
            } else {
                "Record<string, any>".to_string()
            }
        }
        _ => "any".to_string(),
    }
}
