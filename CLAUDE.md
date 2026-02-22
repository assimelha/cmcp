# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

`cmcp` (Code Mode MCP) is a Rust CLI and MCP proxy server that aggregates multiple MCP servers behind just two tools: `search()` and `execute()`. AI agents write TypeScript code to discover and call tools across all connected upstream servers. The TypeScript runs in a sandboxed QuickJS engine (via rquickjs). TypeScript is transpiled to JavaScript using oxc (type stripping only, no full compilation).

## Build & Development

```bash
cargo build                    # debug build
cargo install --path .         # release install
cargo test                     # all tests (7 unit tests, inline in source)
cargo test test_name           # single test by name substring
cargo test catalog::           # tests in a module
cargo test -- --nocapture      # show stdout in tests
cargo clippy                   # lint
```

Requires Rust 1.85+ (edition 2024). No rust-toolchain.toml — uses whatever stable toolchain is installed. No rustfmt.toml or clippy.toml — all defaults apply.

## Architecture

```
Claude/Codex  ──stdio──▶  CodeModeServer (server.rs)
                              │
                 search()/execute()
                              │
              ┌───────────────┼───────────────┐
              ▼               ▼               ▼
         Sandbox         ClientPool        Catalog
       (sandbox.rs)      (client.rs)     (catalog.rs)
       QuickJS runtime   Upstream MCP     Aggregated tool
       + Transpile       connections      list + TS type
       (transpile.rs)    (http/stdio/sse) declarations
```

**Key flow:** Agent TS code → oxc strips types → QuickJS executes JS → calls upstream MCP tools via ClientPool.

**Source files** (all in `src/`):
- `main.rs` — CLI entry point with Clap subcommands, all `cmd_*` handler functions
- `server.rs` — MCP server exposing `search`+`execute`, hot-reload via config mtime checking
- `client.rs` — `ClientPool` connecting to upstream MCP servers, one reconnect retry on failure
- `catalog.rs` — Aggregates tools from all servers, generates TS type declarations from JSON Schema
- `sandbox.rs` — QuickJS sandbox, wraps agent code in async function, provides `call_tool` bridge
- `transpile.rs` — oxc-based TS→JS (strips types only)
- `config.rs` — TOML config types, scope enum (Local/User/Project), load/save/merge logic
- `import.rs` — Discovers servers from Claude/Codex config files

## Key Patterns

- **Error handling:** `anyhow::Result<()>` everywhere, `.context()` for wrapping errors. Non-fatal errors (e.g. upstream connection failure) use `tracing::warn!` and continue.
- **Async:** tokio with `#[tokio::main]`, `Arc<Mutex<T>>` for shared mutable state.
- **Hot-reload:** `CodeModeServer` checks config file mtimes on every request via `maybe_reload()`, reconnects all servers if changed.
- **Environment variables:** Values prefixed with `env:` (e.g. `env:MY_TOKEN`) are resolved at runtime via `resolve_env()`.
- **Server name sanitization:** Hyphens converted to underscores for JS identifier compatibility.
- **Config scopes:** Local/User (`~/.config/code-mode-mcp/config.toml`) and Project (`.cmcp.toml`), merged with project overriding user.
- **Logging:** `tracing` to stderr, controlled via `RUST_LOG` env var.

## CLI Commands

`cmcp add`, `cmcp remove`, `cmcp list`/`ls`, `cmcp install`, `cmcp uninstall`, `cmcp import`, `cmcp serve` (internal), plus `cmcp claude mcp add` / `cmcp codex mcp add` passthrough commands that accept copy-pasted Claude/Codex CLI syntax.
