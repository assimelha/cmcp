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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use anyhow::Result;
use tokio::sync::{Mutex, RwLock};
use tracing::info;

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

/// Mutable state that is updated incrementally as servers connect.
struct ProxyState {
    sandbox: Sandbox,
    catalog: Arc<RwLock<Catalog>>,
    _pool: Arc<ClientPool>,
}

/// The core proxy engine that manages upstream MCP server connections
/// and executes agent-written TypeScript code against them.
///
/// Servers are loaded incrementally: `search`/`execute` return results
/// immediately with whatever servers are ready, while remaining servers
/// continue connecting in the background.
#[derive(Clone)]
pub struct ProxyEngine {
    state: Arc<Mutex<ProxyState>>,
    generation: Arc<AtomicU64>,
    pending: Arc<AtomicUsize>,
    total_servers: Arc<AtomicUsize>,
    failed: Arc<Mutex<Vec<(String, String)>>>,
}

impl ProxyEngine {
    /// Create a ProxyEngine from a map of server configs.
    /// Connects to all configured servers and builds the tool catalog.
    /// Servers that fail to connect are skipped with a warning.
    pub async fn from_configs(servers: HashMap<String, ServerConfig>) -> Result<Self> {
        let state = ProxyState::new(servers).await?;
        Ok(Self {
            state: Arc::new(Mutex::new(state)),
            generation: Arc::new(AtomicU64::new(0)),
            pending: Arc::new(AtomicUsize::new(0)),
            total_servers: Arc::new(AtomicUsize::new(0)),
            failed: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Create a ProxyEngine with an empty catalog, ready for incremental loading.
    /// `total_servers` is the number of servers that will be loaded in the background.
    pub async fn starting(total_servers: usize) -> Self {
        let pool = Arc::new(ClientPool::new());
        let catalog = Arc::new(RwLock::new(Catalog::new()));
        let sandbox = Sandbox::new(pool.clone(), catalog.clone()).await
            .expect("failed to create sandbox");

        let state = ProxyState {
            sandbox,
            catalog,
            _pool: pool,
        };

        Self {
            state: Arc::new(Mutex::new(state)),
            generation: Arc::new(AtomicU64::new(0)),
            pending: Arc::new(AtomicUsize::new(total_servers)),
            total_servers: Arc::new(AtomicUsize::new(total_servers)),
            failed: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Start loading upstream servers in background tasks (one per server).
    /// Each server connects concurrently; search/execute become usable as
    /// each one completes.
    pub fn start_background_load(self: &Arc<Self>, servers: HashMap<String, ServerConfig>) {
        let load_generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.pending.store(servers.len(), Ordering::SeqCst);

        for (name, config) in servers {
            let engine = self.clone();
            let expected_gen = load_generation;
            tokio::spawn(async move {
                let result = ClientPool::connect_one(&name, &config).await;
                if engine.generation.load(Ordering::SeqCst) != expected_gen {
                    return;
                }
                match result {
                    Ok((service, tools)) => {
                        info!(server = %name, tool_count = tools.len(), "connected");
                        let state = engine.state.lock().await;
                        state._pool.add_server(name.clone(), service, config).await;
                        state.catalog.write().await.add_server_tools(&name, tools);
                    }
                    Err(e) => {
                        tracing::warn!(server = %name, error = %e, "failed to connect, skipping");
                        engine.failed.lock().await.push((name, e.to_string()));
                    }
                }
                engine.pending.fetch_sub(1, Ordering::SeqCst);
            });
        }
    }

    /// Build a status suffix describing pending/failed servers.
    async fn status_suffix(&self) -> String {
        let pending = self.pending.load(Ordering::SeqCst);
        let total = self.total_servers.load(Ordering::SeqCst);
        if pending > 0 {
            let ready = total.saturating_sub(pending);
            format!("\n\n[{ready} of {total} server(s) connected; {pending} still loading — retry for more tools]")
        } else {
            let failed = self.failed.lock().await;
            if failed.is_empty() {
                String::new()
            } else {
                let details: Vec<String> = failed
                    .iter()
                    .map(|(name, err)| format!("{name}: {err}"))
                    .collect();
                format!("\n\n[failed servers: {}]", details.join(", "))
            }
        }
    }

    /// Execute a search query — agent TypeScript code that filters the tool catalog.
    pub async fn search(&self, code: &str, max_length: Option<usize>) -> Result<serde_json::Value> {
        let max_len = max_length.unwrap_or(DEFAULT_MAX_LENGTH);
        let state = self.state.lock().await;
        let result = state.sandbox.search(code).await?;
        let mut text = serde_json::to_string_pretty(&result)?;
        let suffix = self.status_suffix().await;
        if !suffix.is_empty() {
            text.push_str(&suffix);
        }
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

        let images = extract_images(&mut result);

        let mut text = serde_json::to_string_pretty(&result)?;
        let suffix = self.status_suffix().await;
        if !suffix.is_empty() {
            text.push_str(&suffix);
        }
        let truncated = truncate_response(text, max_len);

        Ok(ExecuteResult {
            text: truncated,
            images,
        })
    }

    /// Reload the proxy with a new set of server configs.
    /// Resets state and spawns concurrent background load for each server.
    pub async fn reload(&self, servers: HashMap<String, ServerConfig>) -> Result<()> {
        let reload_generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let total = servers.len();

        let pool = Arc::new(ClientPool::new());
        let catalog = Arc::new(RwLock::new(Catalog::new()));
        let sandbox = Sandbox::new(pool.clone(), catalog.clone()).await?;

        {
            let mut state = self.state.lock().await;
            if self.generation.load(Ordering::SeqCst) == reload_generation {
                *state = ProxyState {
                    sandbox,
                    catalog,
                    _pool: pool,
                };
            }
        }

        self.total_servers.store(total, Ordering::SeqCst);
        self.pending.store(total, Ordering::SeqCst);
        self.failed.lock().await.clear();

        let engine: Arc<Self> = Arc::new(self.clone());
        for (name, config) in servers {
            let eng = engine.clone();
            let expected_gen = reload_generation;
            tokio::spawn(async move {
                let result = ClientPool::connect_one(&name, &config).await;
                if eng.generation.load(Ordering::SeqCst) != expected_gen {
                    return;
                }
                match result {
                    Ok((service, tools)) => {
                        info!(server = %name, tool_count = tools.len(), "reconnected");
                        let state = eng.state.lock().await;
                        state._pool.add_server(name.clone(), service, config).await;
                        state.catalog.write().await.add_server_tools(&name, tools);
                    }
                    Err(e) => {
                        tracing::warn!(server = %name, error = %e, "failed to connect, skipping");
                        eng.failed.lock().await.push((name, e.to_string()));
                    }
                }
                eng.pending.fetch_sub(1, Ordering::SeqCst);
            });
        }

        Ok(())
    }

    /// Get a summary of the connected servers and tools.
    pub async fn summary(&self) -> String {
        let pending = self.pending.load(Ordering::SeqCst);
        let total = self.total_servers.load(Ordering::SeqCst);
        let state = self.state.lock().await;
        let catalog = state.catalog.read().await;
        let mut s = catalog.summary();
        if pending > 0 {
            let ready = total.saturating_sub(pending);
            s = format!("{s} ({ready} of {total} server(s) connected, {pending} still loading)");
        }
        s
    }

    /// Get the number of tools in the catalog.
    pub async fn tool_count(&self) -> usize {
        let state = self.state.lock().await;
        let catalog = state.catalog.read().await;
        catalog.entries().len()
    }

    /// Get tool names grouped by server, sorted alphabetically.
    pub async fn catalog_entries_by_server(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        let state = self.state.lock().await;
        let catalog = state.catalog.read().await;
        let mut servers: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for entry in catalog.entries() {
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
        let catalog = Arc::new(RwLock::new(catalog));
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


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::time::{sleep, timeout};

    #[tokio::test]
    async fn starting_search_returns_empty_with_pending_note() {
        let engine = ProxyEngine::starting(3).await;

        let result = engine
            .search("return tools;", None)
            .await
            .expect("search should succeed immediately with incremental loading");

        assert!(result.to_string().contains("still loading"));
        assert!(engine.pending.load(Ordering::SeqCst) == 3);
    }

    #[tokio::test]
    async fn starting_execute_returns_empty_with_pending_note() {
        let engine = ProxyEngine::starting(2).await;

        let result = engine
            .execute("return null;", None)
            .await
            .expect("execute should succeed immediately with incremental loading");

        assert!(result.text.contains("still loading"));
        assert!(engine.pending.load(Ordering::SeqCst) == 2);
    }

    #[tokio::test]
    async fn background_load_empty_config_succeeds_immediately() {
        let engine = Arc::new(ProxyEngine::starting(0).await);
        engine.start_background_load(HashMap::new());

        let result = timeout(Duration::from_secs(2), async {
            engine.search("return tools;", None).await
        })
        .await
        .expect("search should complete immediately")
        .expect("search should not error");

        assert_eq!(result, json!([]));
        assert_eq!(engine.tool_count().await, 0);
    }

    #[tokio::test]
    async fn pending_count_decrements_on_empty_load() {
        let engine = Arc::new(ProxyEngine::starting(0).await);
        assert_eq!(engine.pending.load(Ordering::SeqCst), 0);

        engine.start_background_load(HashMap::new());

        timeout(Duration::from_secs(2), async {
            loop {
                if engine.pending.load(Ordering::SeqCst) == 0 {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("pending count should reach 0");
    }
}