//! cmcp-core: Code-mode MCP proxy library.
//!
//! Aggregates multiple MCP servers behind a TypeScript sandbox,
//! exposing `search()` and `execute()` operations.

pub mod catalog;
pub mod client;
pub mod config;
pub mod sandbox;
pub mod transpile;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;

use catalog::Catalog;
use client::ClientPool;
use config::ServerConfig;
use sandbox::Sandbox;

/// Default max response length in characters (~10k tokens).
const DEFAULT_MAX_LENGTH: usize = 40_000;

/// Image data extracted from an MCP tool response.
#[derive(Debug, Clone)]
pub struct ImageData {
    pub data: String,
    pub mime_type: String,
}

/// Rich execution result that separates text from binary content.
#[derive(Debug)]
pub struct ExecuteResult {
    /// The JSON text portion (truncated, with image data replaced by placeholders).
    pub text: String,
    /// Extracted image content blocks.
    pub images: Vec<ImageData>,
}

/// Mutable state that gets replaced atomically on reload.
/// `pool` is kept alive here — the Sandbox holds its own Arc<ClientPool>
/// reference for tool calls, but we retain ownership for lifecycle management.
struct ProxyState {
    sandbox: Sandbox,
    catalog: Arc<Catalog>,
    _pool: Arc<ClientPool>,
}

/// The core proxy engine that manages upstream MCP server connections
/// and executes agent-written TypeScript code against them.
pub struct ProxyEngine {
    state: Mutex<ProxyState>,
}

impl ProxyEngine {
    /// Create a ProxyEngine from a map of server configs.
    /// Connects to all configured servers and builds the tool catalog.
    /// Servers that fail to connect are skipped with a warning.
    pub async fn from_configs(servers: HashMap<String, ServerConfig>) -> Result<Self> {
        let state = ProxyState::new(servers).await?;
        Ok(Self {
            state: Mutex::new(state),
        })
    }

    /// Execute a search query — agent TypeScript code that filters the tool catalog.
    pub async fn search(&self, code: &str, max_length: Option<usize>) -> Result<serde_json::Value> {
        let max_len = max_length.unwrap_or(DEFAULT_MAX_LENGTH);
        let state = self.state.lock().await;
        let result = state.sandbox.search(code).await?;
        let text = serde_json::to_string_pretty(&result)?;
        let truncated = truncate_response(text, max_len);
        serde_json::from_str(&truncated).or(Ok(serde_json::Value::String(truncated)))
    }

    /// Execute tool-calling code — agent TypeScript that calls tools across servers.
    ///
    /// Extracts image content blocks from the JSON result before truncation,
    /// so binary data is preserved intact.
    pub async fn execute(&self, code: &str, max_length: Option<usize>) -> Result<ExecuteResult> {
        let max_len = max_length.unwrap_or(DEFAULT_MAX_LENGTH);
        let state = self.state.lock().await;
        let mut result = state.sandbox.execute(code).await?;

        // Extract images before truncation so base64 data isn't corrupted.
        let images = extract_images(&mut result);

        let text = serde_json::to_string_pretty(&result)?;
        let truncated = truncate_response(text, max_len);

        Ok(ExecuteResult {
            text: truncated,
            images,
        })
    }

    /// Reload the proxy with a new set of server configs.
    /// Reconnects to all servers and rebuilds the catalog and sandbox.
    pub async fn reload(&self, servers: HashMap<String, ServerConfig>) -> Result<()> {
        let new_state = ProxyState::new(servers).await?;
        let mut state = self.state.lock().await;
        *state = new_state;
        Ok(())
    }

    /// Get a summary of the connected servers and tools.
    pub async fn summary(&self) -> String {
        let state = self.state.lock().await;
        state.catalog.summary()
    }

    /// Get the number of tools in the catalog.
    pub async fn tool_count(&self) -> usize {
        let state = self.state.lock().await;
        state.catalog.entries().len()
    }

    /// Get tool names grouped by server, sorted alphabetically.
    pub async fn catalog_entries_by_server(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        let state = self.state.lock().await;
        let mut servers: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for entry in state.catalog.entries() {
            servers
                .entry(entry.server.clone())
                .or_default()
                .push(entry.name.clone());
        }
        servers
    }
}

impl ProxyState {
    async fn new(servers: HashMap<String, ServerConfig>) -> Result<Self> {
        let (pool, catalog) = ClientPool::connect(servers).await?;
        let catalog = Arc::new(catalog);
        let pool = Arc::new(pool);
        let sandbox = Sandbox::new(pool.clone(), catalog.clone()).await?;
        Ok(Self {
            sandbox,
            catalog,
            _pool: pool,
        })
    }
}

/// Truncate a response to `max_len` characters, appending a notice if truncated.
pub fn truncate_response(text: String, max_len: usize) -> String {
    if max_len == 0 || text.len() <= max_len {
        return text;
    }
    let cut = text[..max_len].rfind('\n').unwrap_or(max_len);
    let truncated = &text[..cut];
    let remaining = text.len() - cut;
    format!(
        "{truncated}\n\n[truncated — {remaining} chars omitted. Use your code to extract only the data you need, or increase max_length.]"
    )
}

/// Recursively walk a JSON value and extract MCP image content blocks.
///
/// Looks for objects matching `{"type": "image", "data": "...", "mimeType": "..."}`.
/// Extracted images are removed from the JSON (data replaced with a placeholder)
/// so the remaining text can be safely truncated without corrupting binary data.
fn extract_images(value: &mut serde_json::Value) -> Vec<ImageData> {
    let mut images = Vec::new();
    extract_images_recursive(value, &mut images);
    images
}

fn extract_images_recursive(value: &mut serde_json::Value, images: &mut Vec<ImageData>) {
    match value {
        serde_json::Value::Object(map) => {
            // Check if this object is an MCP image content block.
            let is_image = map
                .get("type")
                .and_then(|v| v.as_str())
                .is_some_and(|t| t == "image");

            if is_image {
                if let (Some(data), Some(mime_type)) = (
                    map.get("data").and_then(|v| v.as_str()).map(String::from),
                    map.get("mimeType")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                ) {
                    let idx = images.len();
                    images.push(ImageData { data, mime_type });
                    // Replace the data with a placeholder to keep the JSON structure
                    // but avoid truncating the base64 blob.
                    map.insert(
                        "data".to_string(),
                        serde_json::Value::String(format!("[image #{idx} extracted]")),
                    );
                }
            }

            // Recurse into all values.
            for v in map.values_mut() {
                extract_images_recursive(v, images);
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr.iter_mut() {
                extract_images_recursive(item, images);
            }
        }
        _ => {}
    }
}
