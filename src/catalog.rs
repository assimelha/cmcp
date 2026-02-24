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
            // Sanitize server names: hyphens become underscores (matches sandbox proxy names).
            let js_name = server.replace('-', "_");
            if !is_valid_js_ident(&js_name) {
                continue;
            }

            out.push_str(&format!("declare const {js_name}: {{\n"));
            for tool in tools {
                let params_type = schema_to_ts_params(&tool.input_schema);
                // Sanitize description for JSDoc (escape */ sequences).
                let desc = tool.description.replace('\n', " ").replace("*/", "* /");
                if !desc.is_empty() {
                    out.push_str(&format!("  /** {desc} */\n"));
                }
                // Quote tool names that aren't valid identifiers.
                let prop_name = js_property_name(&tool.name);
                let name_str = format!("{prop_name}(params: {{ {params_type} }}): Promise<any>;");
                out.push_str(&format!("  {name_str}\n"));
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
        // Quote property names that aren't valid JS identifiers.
        let name_str = format!("{}{optional}", js_property_name(name));
        params.push(format!("{name_str}: {ts_type}"));
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

fn js_property_name(name: &str) -> String {
    if is_valid_js_ident(name) { name.to_string() } else { format!("\"{name}\"") }
}

/// Check if a string is a valid JavaScript identifier (simplified).
fn is_valid_js_ident(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' && first != '$' {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(server: &str, name: &str, desc: &str, schema: serde_json::Value) -> CatalogEntry {
        CatalogEntry {
            server: server.to_string(),
            name: name.to_string(),
            description: desc.to_string(),
            input_schema: schema,
        }
    }

    #[test]
    fn test_type_declarations_basic() {
        let mut catalog = Catalog::new();
        catalog.entries = vec![
            make_entry("my-server", "navigate", "Navigate to URL", serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string"}
                },
                "required": ["url"]
            })),
        ];

        let decls = catalog.type_declarations();
        assert!(decls.contains("declare const my_server:"), "decls: {decls}");
        assert!(decls.contains("navigate(params:"), "decls: {decls}");
        assert!(decls.contains("url: string"), "decls: {decls}");
    }

    #[test]
    fn test_type_declarations_hyphenated_params() {
        let mut catalog = Catalog::new();
        catalog.entries = vec![
            make_entry("browser", "set_header", "Set a header", serde_json::json!({
                "type": "object",
                "properties": {
                    "content-type": {"type": "string"},
                    "user-agent": {"type": "string"},
                    "x-custom-header": {"type": "string"}
                },
                "required": ["content-type"]
            })),
        ];

        let decls = catalog.type_declarations();
        // Hyphenated property names must be quoted
        assert!(decls.contains("\"content-type\":"), "decls: {decls}");
        assert!(decls.contains("\"user-agent\"?:"), "decls: {decls}");
        assert!(decls.contains("\"x-custom-header\"?:"), "decls: {decls}");
    }

    #[test]
    fn test_type_declarations_transpile_roundtrip() {
        // Build a realistic catalog with edge cases and verify it transpiles cleanly.
        let mut catalog = Catalog::new();
        catalog.entries = vec![
            make_entry("chrome-devtools", "navigate", "Navigate to a URL", serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string"},
                    "referrer": {"type": "string"},
                    "transition-type": {
                        "type": "string",
                        "enum": ["link", "typed", "reload"]
                    }
                },
                "required": ["url"]
            })),
            make_entry("chrome-devtools", "take_screenshot", "Capture screenshot", serde_json::json!({
                "type": "object",
                "properties": {
                    "format": {"type": "string", "enum": ["png", "jpeg"]},
                    "quality": {"type": "integer"},
                    "clip": {
                        "type": "object",
                        "properties": {
                            "x": {"type": "number"},
                            "y": {"type": "number"},
                            "width": {"type": "number"},
                            "height": {"type": "number"}
                        },
                        "required": ["x", "y", "width", "height"]
                    }
                }
            })),
            make_entry("canva", "create_design", "Create a new design", serde_json::json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string"},
                    "width": {"type": "number"},
                    "height": {"type": "number"},
                    "tags": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["title"]
            })),
        ];

        let type_decls = catalog.type_declarations();

        // Wrap agent code with type declarations and transpile
        let agent_code = "return tools.filter(t => t.name.includes(\"screenshot\"))";
        let ts_source = format!(
            "{type_decls}\nasync function __agent__() {{\n{agent_code}\n}}"
        );

        let result = crate::transpile::ts_to_js(&ts_source);
        assert!(result.is_ok(), "transpile failed: {:?}\n\nInput:\n{ts_source}", result.err());
        let js = result.unwrap();
        assert!(js.contains("return tools.filter"), "output: {js}");
    }

    #[test]
    fn test_type_declarations_no_properties() {
        let mut catalog = Catalog::new();
        catalog.entries = vec![
            make_entry("server", "no_args_tool", "A tool with no params", serde_json::json!({
                "type": "object"
            })),
            make_entry("server", "empty_props_tool", "Empty properties", serde_json::json!({
                "type": "object",
                "properties": {}
            })),
        ];

        let decls = catalog.type_declarations();
        // Both should produce valid type declarations
        let ts_source = format!(
            "{decls}\nasync function __agent__() {{\nreturn tools\n}}"
        );
        let result = crate::transpile::ts_to_js(&ts_source);
        assert!(result.is_ok(), "transpile failed: {:?}\n\nInput:\n{ts_source}", result.err());
    }
}
