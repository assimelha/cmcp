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

    /// Install cmcp into Claude (registers the MCP server).
    Install {
        /// Scope: "local" (this machine, default), "user" (global), or "project"
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

    /// Uninstall cmcp from Claude.
    Uninstall,

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

        Commands::Install { scope } => cmd_install(cli.config.as_ref(), &scope),

        Commands::Uninstall => cmd_uninstall(),

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

fn parse_server_args(
    transport: Option<String>,
    auth: Option<String>,
    headers: Vec<String>,
    envs: Vec<String>,
    args: &[String],
) -> Result<ServerConfig> {
    let transport = transport.unwrap_or_else(|| {
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
                .skip_while(|a| *a == "--")
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

fn cmd_install(config_path: Option<&PathBuf>, scope: &str) -> Result<()> {
    let cmcp_bin = std::env::current_exe()
        .context("could not determine cmcp binary path")?;

    let config_path = config_path
        .cloned()
        .unwrap_or_else(|| config::default_config_path().unwrap());

    // Match Claude's scopes exactly: local, user, project
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

    println!("Registering with Claude ({scope})...\n");

    // Try to run it automatically
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .env_remove("CLAUDECODE")
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("Installed! Restart Claude to pick it up.");
        }
        _ => {
            println!("Could not run automatically. Run this manually:\n");
            println!("  {cmd}");
        }
    }

    Ok(())
}

fn cmd_uninstall() -> Result<()> {
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg("claude mcp remove code-mode-mcp")
        .status();

    match status {
        Ok(s) if s.success() => println!("Uninstalled code-mode-mcp from Claude."),
        _ => println!("Run manually: claude mcp remove code-mode-mcp"),
    }
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
