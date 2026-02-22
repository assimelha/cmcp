use std::sync::Arc;

use anyhow::Result;
use rquickjs::context::EvalOptions;
use rquickjs::prelude::Async;
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Function, Promise, Value, async_with};
use crate::catalog::Catalog;
use crate::client::ClientPool;
use crate::transpile;

/// JS sandbox that executes agent-written code with proxied MCP tool calls.
pub struct Sandbox {
    #[allow(dead_code)]
    rt: AsyncRuntime,
    ctx: AsyncContext,
    pool: Arc<ClientPool>,
    catalog: Arc<Catalog>,
}

fn eval_opts() -> EvalOptions {
    let mut opts = EvalOptions::default();
    opts.global = true;
    opts.strict = false;
    // promise = false: our code is already wrapped in (async () => { ... })()
    // which returns a Promise. Setting promise = true adds JS_EVAL_FLAG_ASYNC
    // which double-wraps the result, causing Promise<Promise<T>> instead of Promise<T>.
    opts.promise = false;
    opts
}

/// JS code that defines console.log/warn/error/info, writing to __stderr.
const CONSOLE_SHIM: &str = r#"
const console = {
  _write(level, args) {
    const msg = args.map(a => {
      if (typeof a === 'string') return a;
      try { return JSON.stringify(a); } catch { return String(a); }
    }).join(' ');
    __stderr(level + ': ' + msg);
  },
  log(...args)   { this._write('LOG', args); },
  info(...args)  { this._write('INFO', args); },
  warn(...args)  { this._write('WARN', args); },
  error(...args) { this._write('ERROR', args); },
  debug(...args) { this._write('DEBUG', args); },
};
"#;

impl Sandbox {
    pub async fn new(pool: Arc<ClientPool>, catalog: Arc<Catalog>) -> Result<Self> {
        let rt = AsyncRuntime::new()?;
        rt.set_memory_limit(64 * 1024 * 1024).await; // 64 MB
        let ctx = AsyncContext::full(&rt).await?;

        // Install console shim once on the global context.
        async_with!(ctx => |ctx| {
            // __stderr: native function that writes to Rust stderr
            let stderr_fn = Function::new(ctx.clone(), |msg: String| {
                eprintln!("[js] {msg}");
            })
            .map_err(|e| anyhow::anyhow!("failed to create __stderr: {e}"))?;

            ctx.globals().set("__stderr", stderr_fn)
                .map_err(|e| anyhow::anyhow!("failed to set __stderr: {e}"))?;

            ctx.eval::<(), _>(CONSOLE_SHIM)
                .catch(&ctx)
                .map_err(|e| anyhow::anyhow!("failed to install console shim: {e}"))?;

            Ok::<_, anyhow::Error>(())
        })
        .await?;

        Ok(Self {
            rt,
            ctx,
            pool,
            catalog,
        })
    }

    /// Execute a `search()` call — agent TypeScript code that filters the tool catalog.
    pub async fn search(&self, code: &str) -> Result<serde_json::Value> {
        let catalog_json_str = serde_json::to_string(&self.catalog.to_json_value())?;
        let code = transpile_agent_code(code, &self.catalog.type_declarations())?;

        let result = async_with!(self.ctx => |ctx| {
            let tools_val: Value = ctx.json_parse(catalog_json_str)
                .catch(&ctx)
                .map_err(|e| anyhow::anyhow!("failed to parse catalog: {e}"))?;

            ctx.globals().set("tools", tools_val)
                .map_err(|e| anyhow::anyhow!("failed to set tools: {e}"))?;

            let wrapped = format!("(async () => {{ {code} }})()", code = code);

            let promise: Promise = ctx.eval_with_options(wrapped, eval_opts())
                .catch(&ctx)
                .map_err(|e| anyhow::anyhow!("JS eval error: {e}"))?;

            let result: Value = promise.into_future::<Value>()
                .await
                .catch(&ctx)
                .map_err(|e| anyhow::anyhow!("JS promise rejected: {e}"))?;

            stringify_result(&ctx, result)
        })
        .await?;

        Ok(result)
    }

    /// Execute an `execute()` call — agent TypeScript code that calls tools across servers.
    pub async fn execute(&self, code: &str) -> Result<serde_json::Value> {
        let pool = self.pool.clone();
        let catalog = self.catalog.clone();
        let code = transpile_agent_code(code, &self.catalog.type_declarations())?;

        let result = async_with!(self.ctx => |ctx| {
            // Inject __call_tool as an async native function.
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

                            match pool_inner.call_tool(&server, &tool, params).await {
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
                // Convert server names with hyphens to valid JS identifiers
                // e.g. "chrome-devtools" -> "chrome_devtools"
                let js_name = name.replace('-', "_");
                setup.push_str(&format!(
                    r#"const {js_name} = new Proxy({{}}, {{
  get(_, tool) {{
    return async (args = {{}}) => {{
      const resultJson = await __call_tool("{name}", tool, JSON.stringify(args));
      try {{ return JSON.parse(resultJson); }} catch {{ return resultJson; }}
    }};
  }}
}});
"#,
                    js_name = js_name,
                    name = name,
                ));
            }

            // Also inject the catalog
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

            stringify_result(&ctx, result)
        })
        .await?;

        Ok(result)
    }
}

/// Convert a JS Value back to serde_json::Value via JSON.stringify.
fn stringify_result<'js>(
    ctx: &rquickjs::Ctx<'js>,
    value: Value<'js>,
) -> Result<serde_json::Value> {
    let json_rq_str = ctx.json_stringify(value)
        .catch(ctx)
        .map_err(|e| anyhow::anyhow!("failed to stringify: {e}"))?;

    let json_std_str = match json_rq_str {
        Some(s) => s.to_string()
            .map_err(|e| anyhow::anyhow!("string conversion: {e}"))?,
        None => "null".to_owned(),
    };

    serde_json::from_str(&json_std_str)
        .map_err(|e| anyhow::anyhow!("JSON parse error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use crate::client::ClientPool;

    async fn test_sandbox() -> Sandbox {
        let (pool, catalog) = ClientPool::connect(HashMap::new()).await.unwrap();
        Sandbox::new(Arc::new(pool), Arc::new(catalog)).await.unwrap()
    }

    #[tokio::test]
    async fn test_execute_basic() {
        let sandbox = test_sandbox().await;
        let result = sandbox.execute("return 1 + 2;").await.unwrap();
        assert_eq!(result, serde_json::json!(3));
    }

    #[tokio::test]
    async fn test_execute_promise_all() {
        let sandbox = test_sandbox().await;
        let result = sandbox.execute(r#"
            const results = await Promise.all([
                Promise.resolve("a"),
                Promise.resolve("b"),
                Promise.resolve("c"),
            ]);
            return results;
        "#).await.unwrap();
        assert_eq!(result, serde_json::json!(["a", "b", "c"]));
    }

    #[tokio::test]
    async fn test_execute_chaining() {
        let sandbox = test_sandbox().await;
        let result = sandbox.execute(r#"
            const a = await Promise.resolve(10);
            const b = await Promise.resolve(a * 2);
            const c = await Promise.resolve(b + 5);
            return c;
        "#).await.unwrap();
        assert_eq!(result, serde_json::json!(25));
    }

    #[tokio::test]
    async fn test_call_tool_nonexistent_server_returns_error() {
        let sandbox = test_sandbox().await;
        let result = sandbox.execute(r#"
            const r = await __call_tool("no_such_server", "some_tool", "{}");
            return JSON.parse(r);
        "#).await.unwrap();
        assert!(result.get("error").is_some());
    }

    #[tokio::test]
    async fn test_promise_all_call_tool_concurrent() {
        // Verify that Promise.all with multiple __call_tool calls all complete
        // without deadlocking. With the old Arc<Mutex<ClientPool>>, concurrent
        // calls would serialize on the pool mutex.
        let sandbox = test_sandbox().await;
        let result = sandbox.execute(r#"
            const results = await Promise.all([
                __call_tool("server_a", "tool1", "{}"),
                __call_tool("server_b", "tool2", "{}"),
                __call_tool("server_c", "tool3", "{}"),
            ]);
            return results.map(r => JSON.parse(r));
        "#).await.unwrap();

        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        // All should return errors (servers don't exist), but none should deadlock.
        for item in arr {
            assert!(item.get("error").is_some());
        }
    }

    #[tokio::test]
    async fn test_promise_all_parallel_timing() {
        // Verify that async operations in Promise.all run concurrently, not sequentially.
        // Uses a raw QuickJS runtime with injected async sleep to measure timing.
        let rt = AsyncRuntime::new().unwrap();
        let ctx = AsyncContext::full(&rt).await.unwrap();

        let start = std::time::Instant::now();

        async_with!(ctx => |ctx| {
            let sleep_fn = Function::new(
                ctx.clone(),
                Async(move |ms: f64| {
                    async move {
                        tokio::time::sleep(tokio::time::Duration::from_millis(ms as u64)).await;
                        "done".to_string()
                    }
                }),
            ).unwrap();
            ctx.globals().set("sleep_ms", sleep_fn).unwrap();

            let code = r#"(async () => {
                const results = await Promise.all([
                    sleep_ms(100),
                    sleep_ms(100),
                    sleep_ms(100),
                ]);
                return results;
            })()"#;

            let promise: Promise = ctx.eval_with_options(code, eval_opts())
                .catch(&ctx)
                .unwrap();

            let _result: Value = promise.into_future::<Value>()
                .await
                .catch(&ctx)
                .unwrap();

            Ok::<_, anyhow::Error>(())
        })
        .await
        .unwrap();

        let elapsed = start.elapsed();
        // 3x 100ms in parallel should be ~100ms. If sequential, ~300ms.
        assert!(
            elapsed.as_millis() < 200,
            "Promise.all took {}ms — expected <200ms for parallel execution",
            elapsed.as_millis()
        );
    }
}

/// Prepend type declarations, wrap in async function, and transpile TypeScript to JavaScript.
///
/// The agent code may contain `return` statements (e.g. `return tools.filter(...)`),
/// so we wrap in `async function __agent__() { ... }` before transpiling. After
/// transpilation we extract the function body for QuickJS to wrap in its own IIFE.
fn transpile_agent_code(code: &str, type_decls: &str) -> Result<String> {
    // Wrap agent code in a function so `return` is valid during transpilation.
    let ts_source = format!(
        "{type_decls}\nasync function __agent__() {{\n{code}\n}}",
    );
    let js = transpile::ts_to_js(&ts_source)
        .map_err(|e| anyhow::anyhow!("TypeScript transpile error: {e}"))?;

    // Extract the function body — everything between first `{` and last `}`.
    // The transpiled output looks like: `async function __agent__() { <body> }`
    // (type declarations are stripped, so only the function remains)
    let body = if let Some(start) = js.find("async function __agent__()") {
        let after_fn = &js[start..];
        if let Some(open) = after_fn.find('{') {
            let inner = &after_fn[open + 1..];
            if let Some(close) = inner.rfind('}') {
                inner[..close].trim().to_string()
            } else {
                inner.trim().to_string()
            }
        } else {
            js
        }
    } else {
        // Fallback: return the full transpiled output.
        js
    };

    Ok(body)
}
