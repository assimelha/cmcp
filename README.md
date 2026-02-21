# cmcp — Code Mode MCP

A proxy that aggregates all your MCP servers behind just **2 tools**: `search()` and `execute()`. Instead of registering dozens of MCP servers with Claude Code, register one.

Inspired by [Cloudflare's code-mode MCP](https://blog.cloudflare.com/code-mode-mcp/).

## How it works

Traditional setup — each server adds its own tools (can be hundreds):

```
claude mcp add canva https://mcp.canva.com/mcp
claude mcp add github -- npx -y @modelcontextprotocol/server-github
claude mcp add filesystem -- npx -y @modelcontextprotocol/server-filesystem /tmp
```

With cmcp — one proxy, two tools:

```
cmcp add canva https://mcp.canva.com/mcp
cmcp add --transport stdio github -- npx -y @modelcontextprotocol/server-github
cmcp install
```

The agent writes TypeScript to discover and call tools across all connected servers:

```ts
// search() — find relevant tools
return tools.filter(t => t.name.includes("design"));

// execute() — call tools with typed parameters
const result = await canva.create_design({ title: "My Design" });
return result;
```

Types are auto-generated from tool schemas and stripped via [oxc](https://oxc.rs) before running in a sandboxed QuickJS engine.

## Install

```bash
cargo install --path .
```

## Usage

### Add servers

```bash
# HTTP (default when a URL is given)
cmcp add canva https://mcp.canva.com/mcp

# With auth token (use env: prefix to read from environment)
cmcp add --auth "env:CANVA_TOKEN" canva https://mcp.canva.com/mcp

# With custom headers
cmcp add --auth "env:TOKEN" -H "X-Api-Key: abc123" myserver https://api.example.com/mcp

# SSE transport
cmcp add --transport sse events https://events.example.com/mcp

# Stdio transport
cmcp add --transport stdio github -- npx -y @modelcontextprotocol/server-github

# Stdio with environment variables
cmcp add -e GITHUB_TOKEN=env:GITHUB_TOKEN --transport stdio github -- npx -y @modelcontextprotocol/server-github
```

**Note:** Flags (`--auth`, `-H`, `-e`, `--transport`) must come before the server name and URL.

### Import from Claude Code / Codex

Already have MCP servers configured? Import them:

```bash
# Preview what would be imported
cmcp import --dry-run

# Import from all sources (Claude Code + Codex)
cmcp import

# Import from a specific source
cmcp import --from claude
cmcp import --from codex

# Overwrite existing servers
cmcp import --force
```

Scanned locations:

| Source | Files |
|--------|-------|
| Claude Code | `~/.claude.json`, `.mcp.json` |
| Codex | `~/.codex/config.toml`, `.codex/config.toml` |

### Manage servers

```bash
# List servers (names only)
cmcp list --short

# List servers with tools (connects to each)
cmcp list

# Remove a server
cmcp remove canva
```

### Register with Claude Code

```bash
# Local scope (default — this machine)
cmcp install

# User scope (global)
cmcp install --scope user

# Project scope
cmcp install --scope project

# Uninstall
cmcp uninstall
```

## Transports

| Transport | Flag | Use case |
|-----------|------|----------|
| `http` | `--transport http` (default for URLs) | Streamable HTTP MCP servers |
| `sse` | `--transport sse` | Server-Sent Events MCP servers |
| `stdio` | `--transport stdio` | Local process MCP servers |

## Auth

Bearer tokens can be set per-server with `--auth`. Use the `env:` prefix to read from environment variables at runtime:

```bash
cmcp add --auth "env:MY_TOKEN" myserver https://example.com/mcp
```

Custom headers can be added with `-H`:

```bash
cmcp add -H "X-Api-Key: secret" -H "X-Org-Id: 123" myserver https://example.com/mcp
```

## Config

Config is stored at `~/.config/code-mode-mcp/config.toml`:

```toml
[servers.canva]
transport = "http"
url = "https://mcp.canva.com/mcp"
auth = "env:CANVA_TOKEN"

[servers.canva.headers]
X-Custom = "value"

[servers.github]
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]

[servers.github.env]
GITHUB_TOKEN = "env:GITHUB_TOKEN"
```

Use `--config <path>` to specify an alternate config file.

## Limitations

cmcp works best with **stateless tool servers** — servers where you just need to discover and call tools (Canva, GitHub, filesystem, Stripe, etc.).

MCP servers that rely on **Claude Code hooks** (SessionStart, PostToolUse, Stop) or other lifecycle integrations outside the MCP protocol should be registered directly with Claude Code, not proxied through cmcp. Hooks are Claude Code shell commands triggered by events — they don't go through MCP and won't fire when proxied.

## Sandbox

The `search()` and `execute()` tools accept **TypeScript** code. Types are stripped via [oxc](https://oxc.rs) and the resulting JavaScript runs in a QuickJS sandbox.

- **TypeScript support**: Write typed code — type declarations are auto-generated from tool schemas
- **Memory limit**: 64 MB
- **`console.log/warn/error/info/debug`**: Writes to stderr (visible in server logs)
- **Typed server objects**: Each server is a typed global (e.g., `canva.create_design({ title: string })`)
- **`tools` array**: Full tool catalog available for introspection
- **Async/await**: Fully supported

### Auto-generated types

cmcp generates TypeScript declarations from each tool's JSON Schema:

```ts
declare const canva: {
  /** Create a new design */
  create_design(params: { title: string; width?: number; height?: number }): Promise<any>;
  /** List all designs */
  list_designs(params: { limit?: number }): Promise<any>;
};

declare const github: {
  /** Create an issue */
  create_issue(params: { owner: string; repo: string; title: string; body?: string }): Promise<any>;
};
```

This means agents know exactly what parameters each tool accepts when writing `execute()` code.

## License

MIT
