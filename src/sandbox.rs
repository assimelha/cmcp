use std::sync::Arc;

use anyhow::Result;
use rquickjs::context::EvalOptions;
use rquickjs::prelude::Async;
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Function, Promise, Value, async_with};
use tokio::sync::Mutex;

use crate::catalog::Catalog;
use crate::client::ClientPool;

/// JS sandbox that executes agent-written code with proxied MCP tool calls.
pub struct Sandbox {
    #[allow(dead_code)]
    rt: AsyncRuntime,
    ctx: AsyncContext,
    pool: Arc<Mutex<ClientPool>>,
    catalog: Arc<Catalog>,
}

fn eval_opts() -> EvalOptions {
    let mut opts = EvalOptions::default();
    opts.global = true;
    opts.strict = false;
    opts.promise = true;
    opts
}

impl Sandbox {
    pub async fn new(pool: Arc<Mutex<ClientPool>>, catalog: Arc<Catalog>) -> Result<Self> {
        let rt = AsyncRuntime::new()?;
        rt.set_memory_limit(64 * 1024 * 1024).await; // 64 MB
        let ctx = AsyncContext::full(&rt).await?;
        Ok(Self {
            rt,
            ctx,
            pool,
            catalog,
        })
    }

    /// Execute a `search()` call — agent JS code that filters the tool catalog.
    pub async fn search(&self, code: &str) -> Result<serde_json::Value> {
        let catalog_json_str = serde_json::to_string(&self.catalog.to_json_value())?;
        let code = code.to_string();

        let result = async_with!(self.ctx => |ctx| {
            // Inject the tools catalog as a global
            let tools_val: Value = ctx.json_parse(catalog_json_str)
                .catch(&ctx)
                .map_err(|e| anyhow::anyhow!("failed to parse catalog: {e}"))?;

            ctx.globals().set("tools", tools_val)
                .map_err(|e| anyhow::anyhow!("failed to set tools: {e}"))?;

            // Wrap in async IIFE
            let wrapped = format!("(async () => {{ {code} }})()", code = code);

            let promise: Promise = ctx.eval_with_options(wrapped, eval_opts())
                .catch(&ctx)
                .map_err(|e| anyhow::anyhow!("JS eval error: {e}"))?;

            let result: Value = promise.into_future::<Value>()
                .await
                .catch(&ctx)
                .map_err(|e| anyhow::anyhow!("JS promise rejected: {e}"))?;

            // Stringify back to JSON
            let json_rq_str = ctx.json_stringify(result)
                .catch(&ctx)
                .map_err(|e| anyhow::anyhow!("failed to stringify: {e}"))?;

            let json_std_str = match json_rq_str {
                Some(s) => s.to_string()
                    .map_err(|e| anyhow::anyhow!("string conversion: {e}"))?,
                None => "null".to_owned(),
            };

            serde_json::from_str(&json_std_str)
                .map_err(|e| anyhow::anyhow!("JSON parse error: {e}"))
        })
        .await?;

        Ok(result)
    }

    /// Execute an `execute()` call — agent JS code that calls tools across servers.
    pub async fn execute(&self, code: &str) -> Result<serde_json::Value> {
        let pool = self.pool.clone();
        let catalog = self.catalog.clone();
        let code = code.to_string();

        let result = async_with!(self.ctx => |ctx| {
            // Inject __call_tool as an async native function using the Async wrapper.
            // JS: const result = await __call_tool("canva", "create_design", '{"type":"poster"}')
            let pool_ref = pool.clone();
            let call_tool_fn = Function::new(
                ctx.clone(),
                Async({
                    let pool = pool_ref.clone();
                    move |server: String, tool: String, params_json: String| {
                        let pool_inner = pool.clone();
                        async move {
                            let params: serde_json::Value =
                                serde_json::from_str(&params_json)
                                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

                            let pool_guard = pool_inner.lock().await;
                            match pool_guard.call_tool(&server, &tool, params).await {
                                Ok(call_result) => {
                                    serde_json::to_string(&call_result)
                                        .unwrap_or_else(|_| "null".to_owned())
                                }
                                Err(e) => {
                                    format!(r#"{{"error":"{}"}}"#, e.to_string().replace('"', "\\\""))
                                }
                            }
                        }
                    }
                }),
            )
            .map_err(|e| anyhow::anyhow!("failed to create __call_tool: {e}"))?;

            ctx.globals().set("__call_tool", call_tool_fn)
                .map_err(|e| anyhow::anyhow!("failed to set __call_tool: {e}"))?;

            // Build JS proxy objects for each server.
            let mut setup = String::new();

            let mut server_names: Vec<&str> = catalog
                .entries()
                .iter()
                .map(|e| e.server.as_str())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            server_names.sort();

            for name in &server_names {
                setup.push_str(&format!(
                    r#"const {name} = new Proxy({{}}, {{
  get(_, tool) {{
    return async (args = {{}}) => {{
      const resultJson = await __call_tool("{name}", tool, JSON.stringify(args));
      try {{ return JSON.parse(resultJson); }} catch {{ return resultJson; }}
    }};
  }}
}});
"#,
                    name = name,
                ));
            }

            // Also inject the catalog for reference
            let catalog_json_str = serde_json::to_string(&catalog.to_json_value())
                .unwrap_or_else(|_| "[]".to_owned());
            setup.push_str(&format!("const tools = {};", catalog_json_str));

            let wrapped = format!("(async () => {{ {setup}\n{code} }})()", setup = setup, code = code);

            let promise: Promise = ctx.eval_with_options(wrapped, eval_opts())
                .catch(&ctx)
                .map_err(|e| anyhow::anyhow!("JS eval error: {e}"))?;

            let result: Value = promise.into_future::<Value>()
                .await
                .catch(&ctx)
                .map_err(|e| anyhow::anyhow!("JS promise rejected: {e}"))?;

            let json_rq_str = ctx.json_stringify(result)
                .catch(&ctx)
                .map_err(|e| anyhow::anyhow!("failed to stringify: {e}"))?;

            let json_std_str = match json_rq_str {
                Some(s) => s.to_string()
                    .map_err(|e| anyhow::anyhow!("string conversion: {e}"))?,
                None => "null".to_owned(),
            };

            serde_json::from_str(&json_std_str)
                .map_err(|e| anyhow::anyhow!("JSON parse error: {e}"))
        })
        .await?;

        Ok(result)
    }
}
