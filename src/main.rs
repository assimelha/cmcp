mod catalog;
mod client;
mod config;
mod sandbox;
mod server;

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
    ///   cmcp add --transport stdio github -- npx -y @modelcontextprotocol/server-github
    ///   cmcp add --transport stdio fs -- npx -y @modelcontextprotocol/server-filesystem /tmp
    Add {
        /// Transport type (http, stdio, sse). Defaults to http if a URL is given, stdio otherwise.
        #[arg(short, long)]
        transport: Option<String>,

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

    /// Install cmcp into Claude Code (registers the MCP server).
    Install {
        /// Scope: "project" (default) or "global"
        #[arg(short, long, default_value = "project")]
        scope: String,
    },

    /// Uninstall cmcp from Claude Code.
    Uninstall,

    /// Start the MCP server (used internally by Claude Code).
    Serve,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Add {
            transport,
            name,
            args,
        } => cmd_add(cli.config.as_ref(), transport, name, args),

        Commands::Remove { name } => cmd_remove(cli.config.as_ref(), &name),

        Commands::List { short } => cmd_list(cli.config.as_ref(), short).await,

        Commands::Install { scope } => cmd_install(cli.config.as_ref(), &scope),

        Commands::Uninstall => cmd_uninstall(),

        Commands::Serve => cmd_serve(cli.config.as_ref()).await,
    }
}

fn cmd_add(
    config_path: Option<&PathBuf>,
    transport: Option<String>,
    name: String,
    args: Vec<String>,
) -> Result<()> {
    let mut cfg = config::Config::load(config_path)?;

    let server_config = parse_server_args(transport, &args)?;

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

fn parse_server_args(transport: Option<String>, args: &[String]) -> Result<ServerConfig> {
    // Determine transport from explicit flag or by guessing from args
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
                headers: Default::default(),
            })
        }
        "sse" => {
            let url = args
                .first()
                .context("missing URL. Usage: cmcp add --transport sse <name> <url>")?
                .clone();
            Ok(ServerConfig::Sse {
                url,
                headers: Default::default(),
            })
        }
        "stdio" => {
            // args might start with "--" separator from clap trailing_var_arg
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
                env: Default::default(),
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

fn cmd_install(config_path: Option<&PathBuf>, scope: &str) -> Result<()> {
    let cmcp_bin = std::env::current_exe()
        .context("could not determine cmcp binary path")?;

    let config_path = config_path
        .cloned()
        .unwrap_or_else(|| config::default_config_path().unwrap());

    let scope_flag = match scope {
        "global" => "--scope user",
        _ => "--scope project",
    };

    // Build the claude mcp add command
    let cmd = format!(
        "claude mcp add {scope_flag} --transport stdio code-mode-mcp -- {} serve --config {}",
        cmcp_bin.display(),
        config_path.display(),
    );

    println!("Run this to register with Claude Code:\n");
    println!("  {cmd}");
    println!();

    // Try to run it automatically
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("Installed! Restart Claude Code to use code-mode MCP.");
        }
        _ => {
            println!("Could not run automatically. Copy and run the command above.");
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
        Ok(s) if s.success() => println!("Uninstalled code-mode-mcp from Claude Code."),
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
