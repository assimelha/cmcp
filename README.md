# cmcp — Code Mode MCP

Stop registering dozens of MCP servers. Register **one proxy** that aggregates them all behind just **2 tools**.

Your AI agent writes TypeScript to discover and call tools across every connected server — with full type safety, sandboxed execution, and automatic response truncation.

Inspired by [Cloudflare's approach to code-mode MCP](https://blog.cloudflare.com/code-mode-mcp/).

## Why

The MCP tool explosion is real. Every new server adds 5-30 tools to your agent's context. With 6 servers that's potentially 180 tool definitions the model has to parse on every turn.

cmcp flips the model: instead of N tools, you get **2**:

| Tool | Purpose |
|------|---------|
| `search()` | Discover tools by writing TypeScript filter code |
| `execute()` | Call tools across any server with typed async code |

The agent writes code to interact with tools, not JSON blobs. This means:

- **99% fewer tool definitions** in context (2 vs hundreds)
- **Composable** — chain multiple tool calls in a single execution
- **Type-safe** — auto-generated TypeScript declarations from JSON Schema
- **Sandboxed** — code runs in a QuickJS engine with a 64 MB memory limit

## Quick start

```bash
# Install
cargo install --path .

# Add servers (same syntax you already know)
cmcp add canva https://mcp.canva.com/mcp
cmcp add --transport stdio github -- npx -y @modelcontextprotocol/server-github

# Register the proxy with Claude
cmcp install
```

That's it. Restart Claude and you'll see `code-mode-mcp` with the `search` and `execute` tools.

## Copy-paste from any MCP README

Most MCP server docs give you a `claude mcp add` or `codex mcp add` command. Just prepend `cmcp`:

```bash
# Claude syntax — just prepend cmcp
cmcp claude mcp add chrome-devtools --scope user npx chrome-devtools-mcp@latest
cmcp claude mcp add --transport http canva https://mcp.canva.com/mcp

# Codex syntax — same idea
cmcp codex mcp add my-server -- npx docs-server@latest
cmcp codex mcp add api-server --url https://api.example.com --bearer-token-env-var API_TOKEN
```

## How it works

### search — discover tools

The agent writes TypeScript to filter the tool catalog:

```typescript
// Find screenshot-related tools
return tools.filter(t => t.name.includes("screenshot"));

// Find all tools from a specific server
return tools.filter(t => t.server === "chrome-devtools");

// Get a summary of available servers
const servers = [...new Set(tools.map(t => t.server))];
return servers.map(s => ({
  server: s,
  tools: tools.filter(t => t.server === s).map(t => t.name)
}));
```

### execute — call tools

Each server is a typed global object. The agent calls tools with `await`:

```typescript
// Navigate and take a screenshot
await chrome_devtools.navigate_page({ url: "https://example.com" });
const screenshot = await chrome_devtools.take_screenshot({ format: "png" });
return screenshot;

// Chain multiple servers in one call
const design = await canva.create_design({ title: "Q4 Report" });
const issue = await github.create_issue({
  owner: "myorg",
  repo: "designs",
  title: `New design: ${design.id}`
});
return { design: design.id, issue: issue.number };
```

### Auto-generated types

cmcp generates TypeScript declarations from each tool's JSON Schema, so the agent knows exactly what parameters each tool accepts:

```typescript
declare const chrome_devtools: {
  /** Navigate to a URL */
  navigate_page(params: { url: string }): Promise<any>;
  /** Take a screenshot */
  take_screenshot(params: { format?: "png" | "jpeg"; quality?: number }): Promise<any>;
};

declare const canva: {
  /** Create a new design */
  create_design(params: { title: string; width?: number; height?: number }): Promise<any>;
};
```

Types are stripped via [oxc](https://oxc.rs) before execution in the QuickJS sandbox.

## Adding servers

```bash
# HTTP (default when a URL is given)
cmcp add canva https://mcp.canva.com/mcp

# With auth (use env: prefix to read from environment at runtime)
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

Flags (`--auth`, `-H`, `-e`, `--transport`) must come **before** the server name.

### Import from existing configs

Already have MCP servers configured in Claude or Codex? Import them:

```bash
cmcp import --dry-run     # Preview what would be imported
cmcp import               # Import from all sources
cmcp import --from claude # Only from Claude
cmcp import --from codex  # Only from Codex
cmcp import --force       # Overwrite existing servers
```

| Source | Scanned files |
|--------|--------------|
| Claude | `~/.claude.json`, `.mcp.json` |
| Codex  | `~/.codex/config.toml`, `.codex/config.toml` |

### Manage servers

```bash
cmcp list --short   # Names and transports
cmcp list           # Full listing with tools (connects to each server)
cmcp remove canva   # Remove a server
```

## Installing into Claude / Codex

```bash
cmcp install                         # Both Claude and Codex
cmcp install --target claude         # Only Claude
cmcp install --target codex          # Only Codex
cmcp install --target claude --scope user  # Claude user scope (global)

cmcp uninstall                       # Remove from both
cmcp uninstall --target codex        # Remove from one
```

## Scopes

cmcp supports the same scoping as Claude:

| Scope | Config file | Use case |
|-------|-------------|----------|
| `local` (default) | `~/.config/code-mode-mcp/config.toml` | Your personal servers |
| `user` | Same as local | Same as local |
| `project` | `.cmcp.toml` in project root | Project-specific servers |

When serving, both configs are merged (project overrides user). Use `--scope` with `add`, `remove`, or `install`:

```bash
cmcp add --scope project local-server http://localhost:3000/mcp
```

## Transports

| Transport | Flag | When to use |
|-----------|------|-------------|
| `http` | default for URLs | Streamable HTTP MCP servers |
| `sse` | `--transport sse` | Server-Sent Events servers |
| `stdio` | `--transport stdio` (or auto-detected) | Local process servers |

## Auth

Bearer tokens per server with `--auth`. Use `env:` to resolve from environment at runtime:

```bash
cmcp add --auth "env:MY_TOKEN" myserver https://example.com/mcp
```

Custom headers with `-H`:

```bash
cmcp add -H "X-Api-Key: secret" -H "X-Org-Id: 123" myserver https://example.com/mcp
```

## Config format

Stored at `~/.config/code-mode-mcp/config.toml` (or `.cmcp.toml` for project scope):

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

## Response truncation

Large tool results (DOM snapshots, API responses) are automatically truncated to ~40k characters (~10k tokens) to prevent context flooding. Both tools accept an optional `max_length` parameter:

```typescript
// The agent can control truncation per call
// Or better: extract what you need in code
const snapshot = await chrome_devtools.take_snapshot({});
return snapshot.content[0].text.slice(0, 2000);
```

## Limitations

cmcp works best with **stateless tool servers** — servers where you discover and call tools (Canva, GitHub, filesystem, Stripe, browser automation, etc.).

**Not suitable for:**

- **Hook-dependent servers** — MCP servers that rely on Claude hooks (SessionStart, PostToolUse, Stop) for lifecycle management. Hooks are shell commands triggered by Claude events and don't go through MCP, so they won't fire when proxied.
- **Servers requiring interactive auth flows** — OAuth callbacks or browser-based login that need direct Claude integration.

When in doubt, check if the server's README mentions hooks or lifecycle events. If it does, register it directly with Claude instead.

## Requirements

- Rust 1.91+ (for oxc)
- Claude and/or Codex CLI installed

## License

MIT
