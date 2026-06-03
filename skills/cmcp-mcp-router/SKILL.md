---
name: cmcp-mcp-router
description: "Route all MCP tool calls through the code-mode-mcp proxy. Use when MCP tools are needed and the code-mode-mcp server is available. Triggers on any task requiring upstream MCP tool access."
compatibility: Requires cmcp installed as an MCP server
---

# Route MCP through cmcp

When MCP tools are needed and the `code-mode-mcp` server is available, route all MCP work through it.

## Rules

1. Use `search` to discover tools and inspect schemas before calling anything.
2. Use `execute` to invoke upstream MCP tools. Do not call provider-specific MCP servers directly.
3. Do not guess tool names or parameters. Inspect with `search` first.
4. Return only the fields you need. Use the `max_length` parameter on `search` and `execute` (default 40k chars) to limit response size when possible.

## Server global naming

Hyphens in server names become underscores in `execute` code: `my-server` → `my_server`.

## Initialization

If `search` returns an initializing error, upstream servers are still connecting. Retry after a moment — cmcp hot-reloads and does not need a restart.
