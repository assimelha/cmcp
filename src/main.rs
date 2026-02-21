mod catalog;
mod client;
mod config;
mod import;
mod sandbox;
mod server;
mod transpile;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rmcp::transport::stdio;
use rmcp::ServiceExt;
use tracing::info;
use tracing_subscriber::EnvFilter;

use config::ServerConfig;

#[derive(Parser)]
#[command(
    name = "cmcp",
    about = "Code-mode MCP — aggregate all your MCP servers behind search() + execute()",
    version
)]
struct Cli {
    /// Path to config file (default: ~/.config/code-mode-mcp/config.toml)
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Add an MCP server.
    ///
    /// Examples:
    ///   cmcp add canva https://mcp.canva.com/mcp
    ///   cmcp add canva https://mcp.canva.com/mcp --auth env:CANVA_TOKEN
    ///   cmcp add --transport stdio github -- npx -y @modelcontextprotocol/server-github
    ///   cmcp add -e GITHUB_TOKEN=env:GITHUB_TOKEN --transport stdio github -- npx -y @modelcontextprotocol/server-github
    Add {
        /// Transport type (http, stdio, sse). Defaults to http if a URL is given, stdio otherwise.
        #[arg(short, long)]
        transport: Option<String>,

        /// Bearer auth token for http/sse (use "env:VAR" to read from environment).
        #[arg(short, long)]
        auth: Option<String>,

        /// Custom HTTP header (e.g. -H "X-Api-Key: abc123"). Can be repeated.
        #[arg(short = 'H', long = "header")]
        headers: Vec<String>,

        /// Environment variable for stdio (e.g. -e KEY=value). Can be repeated.
        #[arg(short, long = "env")]
        envs: Vec<String>,

        /// Server name (e.g. "canva", "github", "filesystem")
        name: String,

        /// URL (for http/sse) or command (for stdio). For stdio with args, put them after --.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Remove an MCP server.
    Remove {
        /// Server name to remove
        name: String,
    },

    /// List configured servers and their tools.
    #[command(alias = "ls")]
    List {
        /// Only show server names (don't connect to fetch tools)
        #[arg(short, long)]
        short: bool,
    },

    /// Install cmcp into Claude and/or Codex.
    ///
    /// Examples:
    ///   cmcp install                   # install into both Claude and Codex
    ///   cmcp install --target claude   # only Claude
    ///   cmcp install --target codex    # only Codex
    ///   cmcp install --scope user      # Claude user scope
    Install {
        /// Target: "claude", "codex", or omit for both.
        #[arg(short, long)]
        target: Option<String>,

        /// Scope for Claude: "local" (default), "user" (global), or "project".
        #[arg(short, long, default_value = "local")]
        scope: String,
    },

    /// Import MCP servers from Claude or Codex.
    ///
    /// Scans known config locations and adds discovered servers to cmcp.
    ///
    /// Examples:
    ///   cmcp import                    # import from all sources
    ///   cmcp import --from claude      # only from Claude
    ///   cmcp import --from codex       # only from Codex
    ///   cmcp import --dry-run          # preview without writing
    ///   cmcp import --force            # overwrite existing servers
    Import {
        /// Source to import from: "claude", "codex", or omit for all.
        #[arg(short, long)]
        from: Option<String>,

        /// Preview what would be imported without writing.
        #[arg(short, long)]
        dry_run: bool,

        /// Overwrite existing servers with the same name.
        #[arg(long)]
        force: bool,
    },

    /// Uninstall cmcp from Claude and/or Codex.
    Uninstall {
        /// Target: "claude", "codex", or omit for both.
        #[arg(short, long)]
        target: Option<String>,
    },

    /// Passthrough for Claude CLI syntax.
    ///
    /// Copy any `claude mcp add` command and prepend `cmcp`:
    ///   cmcp claude mcp add chrome-devtools --scope user npx chrome-devtools-mcp@latest
    ///   cmcp claude mcp add --transport http canva https://mcp.canva.com/mcp
    #[command(name = "claude")]
    Claude {
        /// Raw arguments (parsed as Claude CLI syntax).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Passthrough for Codex CLI syntax.
    ///
    /// Copy any `codex mcp add` command and prepend `cmcp`:
    ///   cmcp codex mcp add chrome-devtools -- npx chrome-devtools-mcp@latest
    ///   cmcp codex mcp add api-server --url https://api.example.com
    #[command(name = "codex")]
    Codex {
        /// Raw arguments (parsed as Codex CLI syntax).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Start the MCP server (used internally by Claude).
    Serve,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Add {
            transport,
            auth,
            headers,
            envs,
            name,
            args,
        } => cmd_add(cli.config.as_ref(), transport, auth, headers, envs, name, args),

        Commands::Remove { name } => cmd_remove(cli.config.as_ref(), &name),

        Commands::List { short } => cmd_list(cli.config.as_ref(), short).await,

        Commands::Import {
            from,
            dry_run,
            force,
        } => cmd_import(cli.config.as_ref(), from, dry_run, force),

        Commands::Install { target, scope } => cmd_install(cli.config.as_ref(), target.as_deref(), &scope),

        Commands::Uninstall { target } => cmd_uninstall(target.as_deref()),

        Commands::Claude { args } => cmd_passthrough_claude(cli.config.as_ref(), &args),

        Commands::Codex { args } => cmd_passthrough_codex(cli.config.as_ref(), &args),

        Commands::Serve => cmd_serve(cli.config.as_ref()).await,
    }
}

fn cmd_add(
    config_path: Option<&PathBuf>,
    transport: Option<String>,
    auth: Option<String>,
    headers: Vec<String>,
    envs: Vec<String>,
    name: String,
    args: Vec<String>,
) -> Result<()> {
    let mut cfg = config::Config::load(config_path)?;

    let server_config = parse_server_args(transport, auth, headers, envs, &args)?;

    let already_exists = cfg.servers.contains_key(&name);
    cfg.add_server(name.clone(), server_config);
    cfg.save(config_path)?;

    if already_exists {
        println!("Updated server \"{name}\"");
    } else {
        println!("Added server \"{name}\"");
    }

    let path = config_path
        .cloned()
        .unwrap_or_else(|| config::default_config_path().unwrap());
    println!("Config: {}", path.display());
    Ok(())
}

/// Parse "Key: Value" or "Key=Value" header strings into a HashMap.
fn parse_headers(raw: &[String]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for h in raw {
        if let Some((k, v)) = h.split_once(':') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        } else if let Some((k, v)) = h.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

/// Parse "KEY=VALUE" env strings into a HashMap.
fn parse_envs(raw: &[String]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for e in raw {
        if let Some((k, v)) = e.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        }
    }
    map
}

/// Strip Claude/Codex CLI flags that users copy from READMEs but aren't cmcp flags.
/// e.g. `cmcp add chrome-devtools --scope user npx chrome-devtools-mcp@latest`
///       → strips `--scope user`, keeps `npx chrome-devtools-mcp@latest`
fn strip_foreign_flags(args: &[String]) -> (Vec<String>, Option<String>) {
    let mut cleaned = Vec::new();
    let mut extracted_transport = None;
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            // --scope is a Claude CLI flag, not a cmcp flag. Skip it + its value.
            "--scope" => {
                i += 1; // skip the value too
            }
            // --transport may appear in the trailing args if user put it after the name.
            // Extract its value so we can use it.
            "--transport" if i + 1 < args.len() => {
                extracted_transport = Some(args[i + 1].clone());
                i += 1; // skip the value too
            }
            _ => {
                cleaned.push(arg.clone());
            }
        }
        i += 1;
    }

    (cleaned, extracted_transport)
}

fn parse_server_args(
    transport: Option<String>,
    auth: Option<String>,
    headers: Vec<String>,
    envs: Vec<String>,
    args: &[String],
) -> Result<ServerConfig> {
    let (args, trailing_transport) = strip_foreign_flags(args);

    // Use explicitly provided --transport, or one extracted from trailing args, or auto-detect.
    let transport = transport
        .or(trailing_transport)
        .unwrap_or_else(|| {
            if let Some(first) = args.first() {
                if first.starts_with("http://") || first.starts_with("https://") {
                    "http".to_string()
                } else {
                    "stdio".to_string()
                }
            } else {
                "http".to_string()
            }
        });

    match transport.as_str() {
        "http" => {
            let url = args
                .first()
                .context("missing URL. Usage: cmcp add <name> <url>")?
                .clone();
            Ok(ServerConfig::Http {
                url,
                auth,
                headers: parse_headers(&headers),
            })
        }
        "sse" => {
            let url = args
                .first()
                .context("missing URL. Usage: cmcp add --transport sse <name> <url>")?
                .clone();
            Ok(ServerConfig::Sse {
                url,
                auth,
                headers: parse_headers(&headers),
            })
        }
        "stdio" => {
            let cleaned: Vec<String> = args
                .iter()
                .skip_while(|a| a.as_str() == "--")
                .cloned()
                .collect();

            let command = cleaned
                .first()
                .context("missing command. Usage: cmcp add --transport stdio <name> -- <command> [args...]")?
                .clone();

            let cmd_args = cleaned.get(1..).unwrap_or_default().to_vec();

            Ok(ServerConfig::Stdio {
                command,
                args: cmd_args,
                env: parse_envs(&envs),
            })
        }
        other => anyhow::bail!("unknown transport \"{other}\". Use: http, stdio, or sse"),
    }
}

fn cmd_remove(config_path: Option<&PathBuf>, name: &str) -> Result<()> {
    let mut cfg = config::Config::load(config_path)?;

    if cfg.remove_server(name) {
        cfg.save(config_path)?;
        println!("Removed server \"{name}\"");
    } else {
        println!("Server \"{name}\" not found");
    }
    Ok(())
}

async fn cmd_list(config_path: Option<&PathBuf>, short: bool) -> Result<()> {
    let cfg = config::Config::load(config_path)?;

    if cfg.servers.is_empty() {
        println!("No servers configured. Add one with: cmcp add <name> <url>");
        return Ok(());
    }

    if short {
        for (name, server_config) in &cfg.servers {
            let transport_info = match server_config {
                ServerConfig::Http { url, .. } => format!("http  {url}"),
                ServerConfig::Sse { url, .. } => format!("sse   {url}"),
                ServerConfig::Stdio { command, args, .. } => {
                    format!("stdio {} {}", command, args.join(" "))
                }
            };
            println!("  {name:20} {transport_info}");
        }
        return Ok(());
    }

    // Full listing: connect and show tools
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let (_pool, catalog) = client::ClientPool::connect(cfg.servers).await?;

    println!("{}\n", catalog.summary());
    for entry in catalog.entries() {
        println!("  {}.{}", entry.server, entry.name);
        if !entry.description.is_empty() {
            // Truncate long descriptions
            let desc = &entry.description;
            if desc.len() > 100 {
                println!("    {}...", &desc[..100]);
            } else {
                println!("    {desc}");
            }
        }
    }
    Ok(())
}

fn cmd_import(
    config_path: Option<&PathBuf>,
    from: Option<String>,
    dry_run: bool,
    force: bool,
) -> Result<()> {
    let source_filter = match from.as_deref() {
        Some("claude" | "claude-code") => Some(import::ImportSource::ClaudeCode),
        Some("codex" | "openai") => Some(import::ImportSource::Codex),
        Some(other) => anyhow::bail!(
            "unknown source \"{other}\". Use: claude, codex, or omit for all"
        ),
        None => None,
    };

    let discovered = import::discover(source_filter)?;

    if discovered.is_empty() {
        println!("No MCP servers found to import.");
        if source_filter.is_none() {
            println!("\nSearched:");
            println!("  Claude: ~/.claude.json, .mcp.json");
            println!("  Codex:       ~/.codex/config.toml, .codex/config.toml");
        }
        return Ok(());
    }

    let mut cfg = config::Config::load(config_path)?;

    let mut added = 0;
    let mut skipped = 0;
    let mut updated = 0;

    for server in &discovered {
        let exists = cfg.servers.contains_key(&server.name);

        let transport_info = match &server.config {
            ServerConfig::Http { url, .. } => format!("http  {url}"),
            ServerConfig::Sse { url, .. } => format!("sse   {url}"),
            ServerConfig::Stdio { command, args, .. } => {
                format!("stdio {} {}", command, args.join(" "))
            }
        };

        if exists && !force {
            if dry_run {
                println!("  skip  {:<20} {:<12} {} (already exists)", server.name, server.source, transport_info);
            }
            skipped += 1;
        } else if exists && force {
            if dry_run {
                println!("  update {:<19} {:<12} {}", server.name, server.source, transport_info);
            } else {
                cfg.add_server(server.name.clone(), server.config.clone());
            }
            updated += 1;
        } else {
            if dry_run {
                println!("  add   {:<20} {:<12} {}", server.name, server.source, transport_info);
            } else {
                cfg.add_server(server.name.clone(), server.config.clone());
            }
            added += 1;
        }
    }

    if dry_run {
        println!();
        println!("Dry run: {} to add, {} to update, {} to skip", added, updated, skipped);
        println!("Run without --dry-run to apply.");
    } else {
        cfg.save(config_path)?;
        let path = config_path
            .cloned()
            .unwrap_or_else(|| config::default_config_path().unwrap());

        if added > 0 || updated > 0 {
            println!("Imported {} server(s) ({} added, {} updated, {} skipped)", added + updated, added, updated, skipped);
            println!("Config: {}", path.display());
        } else {
            println!("No new servers to import ({} already exist).", skipped);
        }
    }

    Ok(())
}

fn cmd_install(config_path: Option<&PathBuf>, target: Option<&str>, scope: &str) -> Result<()> {
    let cmcp_bin = std::env::current_exe()
        .context("could not determine cmcp binary path")?;

    let config_path = config_path
        .cloned()
        .unwrap_or_else(|| config::default_config_path().unwrap());

    let install_claude = target.is_none() || matches!(target, Some("claude"));
    let install_codex = target.is_none() || matches!(target, Some("codex" | "openai"));

    if let Some(t) = target {
        if !matches!(t, "claude" | "codex" | "openai") {
            anyhow::bail!("unknown target \"{t}\". Use: claude, codex, or omit for both");
        }
    }

    if install_claude {
        install_to_claude(&cmcp_bin, &config_path, scope);
    }

    if install_codex {
        if install_claude {
            println!();
        }
        install_to_codex(&cmcp_bin, &config_path);
    }

    Ok(())
}

fn install_to_claude(cmcp_bin: &std::path::Path, config_path: &std::path::Path, scope: &str) {
    let scope_flag = match scope {
        "user" | "global" => "--scope user",
        "project" => "--scope project",
        _ => "--scope local",
    };

    let cmd = format!(
        "claude mcp add {scope_flag} --transport stdio code-mode-mcp -- {} serve --config {}",
        cmcp_bin.display(),
        config_path.display(),
    );

    println!("Registering with Claude ({scope})...");

    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .env_remove("CLAUDECODE")
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("  Installed in Claude! Restart to pick it up.");
        }
        _ => {
            println!("  Could not run automatically. Run this manually:\n");
            println!("  {cmd}");
        }
    }
}

fn install_to_codex(cmcp_bin: &std::path::Path, config_path: &std::path::Path) {
    println!("Registering with Codex...");

    // Codex uses ~/.codex/config.toml with [mcp_servers.name] sections.
    let codex_config_path = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".codex").join("config.toml"));

    let Some(codex_path) = codex_config_path else {
        println!("  Could not determine Codex config path (HOME not set).");
        println!("  Add manually to ~/.codex/config.toml:\n");
        print_codex_snippet(cmcp_bin, config_path);
        return;
    };

    // Read existing config or start fresh.
    let mut content = if codex_path.exists() {
        std::fs::read_to_string(&codex_path).unwrap_or_default()
    } else {
        String::new()
    };

    // Check if already registered.
    if content.contains("[mcp_servers.code-mode-mcp]") {
        println!("  Already registered in Codex config.");
        return;
    }

    // Append the server config.
    let snippet = format!(
        "\n[mcp_servers.code-mode-mcp]\ncommand = \"{}\"\nargs = [\"serve\", \"--config\", \"{}\"]\n",
        cmcp_bin.display(),
        config_path.display(),
    );

    content.push_str(&snippet);

    if let Some(parent) = codex_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    match std::fs::write(&codex_path, &content) {
        Ok(()) => {
            println!("  Installed in Codex! ({})", codex_path.display());
        }
        Err(e) => {
            println!("  Could not write to {}: {e}", codex_path.display());
            println!("  Add manually:\n");
            print_codex_snippet(cmcp_bin, config_path);
        }
    }
}

fn print_codex_snippet(cmcp_bin: &std::path::Path, config_path: &std::path::Path) {
    println!("  [mcp_servers.code-mode-mcp]");
    println!("  command = \"{}\"", cmcp_bin.display());
    println!(
        "  args = [\"serve\", \"--config\", \"{}\"]",
        config_path.display()
    );
}

fn cmd_uninstall(target: Option<&str>) -> Result<()> {
    let uninstall_claude = target.is_none() || matches!(target, Some("claude"));
    let uninstall_codex = target.is_none() || matches!(target, Some("codex" | "openai"));

    if let Some(t) = target {
        if !matches!(t, "claude" | "codex" | "openai") {
            anyhow::bail!("unknown target \"{t}\". Use: claude, codex, or omit for both");
        }
    }

    if uninstall_claude {
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg("claude mcp remove code-mode-mcp")
            .env_remove("CLAUDECODE")
            .status();

        match status {
            Ok(s) if s.success() => println!("Uninstalled from Claude."),
            _ => println!("Claude: run manually: claude mcp remove code-mode-mcp"),
        }
    }

    if uninstall_codex {
        uninstall_from_codex();
    }

    Ok(())
}

fn uninstall_from_codex() {
    let codex_path = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".codex").join("config.toml"));

    let Some(codex_path) = codex_path else {
        println!("Codex: could not determine config path.");
        return;
    };

    if !codex_path.exists() {
        println!("Codex: no config found.");
        return;
    }

    let content = match std::fs::read_to_string(&codex_path) {
        Ok(c) => c,
        Err(e) => {
            println!("Codex: could not read config: {e}");
            return;
        }
    };

    if !content.contains("[mcp_servers.code-mode-mcp]") {
        println!("Codex: code-mode-mcp not found in config.");
        return;
    }

    // Remove the [mcp_servers.code-mode-mcp] section.
    let mut lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == "[mcp_servers.code-mode-mcp]" {
            let start = i;
            i += 1;
            // Remove until next section header or EOF.
            while i < lines.len() && !lines[i].starts_with('[') {
                i += 1;
            }
            // Also remove trailing blank line.
            lines.drain(start..i);
            // Remove leading blank line if present.
            if start > 0 && start <= lines.len() && lines.get(start.saturating_sub(1)).is_some_and(|l| l.trim().is_empty()) {
                lines.remove(start - 1);
            }
            break;
        }
        i += 1;
    }

    let new_content = lines.join("\n");
    match std::fs::write(&codex_path, &new_content) {
        Ok(()) => println!("Uninstalled from Codex."),
        Err(e) => println!("Codex: could not write config: {e}"),
    }
}

/// Parse `cmcp claude mcp add <name> [--scope S] [--transport T] <url-or-cmd> [args...]`
///
/// Claude CLI syntax: `claude mcp add [--scope S] [--transport T] <name> [--] <url-or-cmd> [args...]`
fn cmd_passthrough_claude(config_path: Option<&PathBuf>, raw_args: &[String]) -> Result<()> {
    // Expect: mcp add [flags...] <name> [--] <cmd-or-url> [args...]
    // Skip leading "mcp" and "add"
    let args: Vec<&str> = raw_args.iter().map(|s| s.as_str()).collect();

    let rest = match args.as_slice() {
        ["mcp", "add", rest @ ..] => rest.to_vec(),
        ["add", rest @ ..] => rest.to_vec(),
        _ => anyhow::bail!(
            "expected: cmcp claude mcp add <name> ...\n\
             Usage: copy a `claude mcp add` command and prepend `cmcp`"
        ),
    };

    // Parse flags and positional args from the Claude syntax.
    let mut transport = None;
    let mut scope = None;
    let mut positional = Vec::new();
    let mut i = 0;

    while i < rest.len() {
        match rest[i] {
            "--transport" | "-t" if i + 1 < rest.len() => {
                transport = Some(rest[i + 1].to_string());
                i += 2;
            }
            "--scope" | "-s" if i + 1 < rest.len() => {
                scope = Some(rest[i + 1].to_string());
                i += 2;
            }
            "--" => {
                // Everything after -- is command args
                positional.extend(rest[i..].iter().map(|s| s.to_string()));
                break;
            }
            arg if arg.starts_with('-') => {
                // Skip unknown flags (e.g. --header, --env) with their values
                if i + 1 < rest.len() && !rest[i + 1].starts_with('-') {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => {
                positional.push(rest[i].to_string());
                i += 1;
            }
        }
    }

    // First positional is the server name, rest is url/command + args.
    let name = positional
        .first()
        .context("missing server name")?
        .clone();
    let cmd_args: Vec<String> = positional[1..].to_vec();

    let server_config = parse_server_args(transport, None, vec![], vec![], &cmd_args)?;

    let mut cfg = config::Config::load(config_path)?;
    let exists = cfg.servers.contains_key(&name);
    cfg.add_server(name.clone(), server_config);
    cfg.save(config_path)?;

    if exists {
        println!("Updated server \"{name}\"");
    } else {
        println!("Added server \"{name}\"");
    }

    if let Some(s) = &scope {
        println!("  (--scope {s} noted — applies to `cmcp install --scope {s}`)");
    }

    let path = config_path
        .cloned()
        .unwrap_or_else(|| config::default_config_path().unwrap());
    println!("Config: {}", path.display());
    Ok(())
}

/// Parse `cmcp codex mcp add <name> [--url U] [--bearer-token-env-var V] [--] <cmd> [args...]`
///
/// Codex CLI syntax: `codex mcp add <name> [--url U] [--env K=V] [--] <cmd> [args...]`
fn cmd_passthrough_codex(config_path: Option<&PathBuf>, raw_args: &[String]) -> Result<()> {
    let args: Vec<&str> = raw_args.iter().map(|s| s.as_str()).collect();

    let rest = match args.as_slice() {
        ["mcp", "add", rest @ ..] => rest.to_vec(),
        ["add", rest @ ..] => rest.to_vec(),
        _ => anyhow::bail!(
            "expected: cmcp codex mcp add <name> ...\n\
             Usage: copy a `codex mcp add` command and prepend `cmcp`"
        ),
    };

    // Parse flags and positional args from the Codex syntax.
    let mut url = None;
    let mut auth = None;
    let mut envs = HashMap::new();
    let mut positional = Vec::new();
    let mut i = 0;

    while i < rest.len() {
        match rest[i] {
            "--url" if i + 1 < rest.len() => {
                url = Some(rest[i + 1].to_string());
                i += 2;
            }
            "--bearer-token-env-var" if i + 1 < rest.len() => {
                auth = Some(format!("env:{}", rest[i + 1]));
                i += 2;
            }
            "--bearer-token" if i + 1 < rest.len() => {
                auth = Some(rest[i + 1].to_string());
                i += 2;
            }
            "--env" if i + 1 < rest.len() => {
                if let Some((k, v)) = rest[i + 1].split_once('=') {
                    envs.insert(k.to_string(), v.to_string());
                }
                i += 2;
            }
            "--" => {
                positional.extend(rest[i + 1..].iter().map(|s| s.to_string()));
                break;
            }
            arg if arg.starts_with('-') => {
                // Skip unknown flags with their values.
                if i + 1 < rest.len() && !rest[i + 1].starts_with('-') {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => {
                positional.push(rest[i].to_string());
                i += 1;
            }
        }
    }

    let name = positional
        .first()
        .context("missing server name")?
        .clone();

    let server_config = if let Some(url) = url {
        // HTTP server
        ServerConfig::Http {
            url,
            auth,
            headers: HashMap::new(),
        }
    } else {
        // Stdio server — remaining positional args are command + args
        let cmd_args: Vec<String> = positional[1..].to_vec();
        let command = cmd_args
            .first()
            .context("missing command")?
            .clone();
        let args = cmd_args.get(1..).unwrap_or_default().to_vec();

        ServerConfig::Stdio {
            command,
            args,
            env: envs,
        }
    };

    let mut cfg = config::Config::load(config_path)?;
    let exists = cfg.servers.contains_key(&name);
    cfg.add_server(name.clone(), server_config);
    cfg.save(config_path)?;

    if exists {
        println!("Updated server \"{name}\"");
    } else {
        println!("Added server \"{name}\"");
    }

    let path = config_path
        .cloned()
        .unwrap_or_else(|| config::default_config_path().unwrap());
    println!("Config: {}", path.display());
    Ok(())
}

async fn cmd_serve(config_path: Option<&PathBuf>) -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cfg = config::Config::load(config_path)?;

    info!(
        server_count = cfg.servers.len(),
        "connecting to upstream servers"
    );

    let (pool, catalog) = client::ClientPool::connect(cfg.servers).await?;
    info!("{}", catalog.summary());

    let server = server::CodeModeServer::new(pool, catalog).await?;

    info!("starting MCP server on stdio");
    let service = server.serve(stdio()).await?;
    service.waiting().await?;

    Ok(())
}
