mod catalog;
mod client;
mod config;
mod sandbox;
mod server;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use rmcp::transport::stdio;
use rmcp::ServiceExt;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "code-mode-mcp",
    about = "A code-mode MCP proxy that aggregates multiple MCP servers behind search() + execute()"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the MCP server (stdio transport, for use with Claude Code).
    Serve {
        /// Path to config file (default: ~/.config/code-mode-mcp/config.toml)
        #[arg(short, long)]
        config: Option<PathBuf>,
    },

    /// List tools from all configured servers (diagnostic).
    List {
        /// Path to config file
        #[arg(short, long)]
        config: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve { config: config_path } => {
            // For serve mode, log to stderr so stdout stays clean for MCP JSON-RPC
            tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_env_filter(EnvFilter::from_default_env())
                .init();

            let config = config::Config::load(config_path.as_ref())?;

            info!(
                server_count = config.servers.len(),
                "connecting to upstream servers"
            );

            let (pool, catalog) = client::ClientPool::connect(config.servers).await?;
            info!("{}", catalog.summary());

            let server = server::CodeModeServer::new(pool, catalog).await?;

            info!("starting MCP server on stdio");
            let service = server.serve(stdio()).await?;
            service.waiting().await?;

            Ok(())
        }

        Commands::List { config: config_path } => {
            tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::from_default_env())
                .init();

            let config = config::Config::load(config_path.as_ref())?;
            let (_pool, catalog) = client::ClientPool::connect(config.servers).await?;

            println!("{}\n", catalog.summary());
            for entry in catalog.entries() {
                println!("  {}.{}", entry.server, entry.name);
                if !entry.description.is_empty() {
                    println!("    {}", entry.description);
                }
            }

            Ok(())
        }
    }
}
