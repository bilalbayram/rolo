use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::{Parser, Subcommand};
use rolo::AppConfig;
use rolo::db::Db;
use rolo::delivery::{self, WorkerConfig};
use rolo::service::api_keys::{self, Scope};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

/// A headless CRM for agents and apps: REST + MCP on a single SQLite file.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Path to the SQLite database file (created if missing).
    #[arg(long, global = true, env = "ROLO_DB", default_value = "rolo.db")]
    db: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP server (REST API at /v1, MCP at /mcp, docs at /docs) and
    /// the webhook delivery worker.
    Serve {
        #[arg(long, env = "ROLO_ADDR", default_value = "127.0.0.1:3000")]
        addr: SocketAddr,
        /// Hostnames the MCP endpoint accepts in the Host header, e.g. crm.example.com.
        /// Defaults to loopback only; set this when exposing rolo beyond localhost.
        #[arg(
            long = "allowed-host",
            env = "ROLO_ALLOWED_HOSTS",
            value_delimiter = ','
        )]
        allowed_hosts: Vec<String>,
        /// Allow webhooks to loopback, private and other internal addresses.
        #[arg(long, env = "ROLO_ALLOW_PRIVATE_WEBHOOKS")]
        allow_private_webhooks: bool,
    },
    /// Manage API keys.
    #[command(subcommand)]
    Keys(KeysCommand),
    /// Write a consistent snapshot of the database to a new file. Safe while
    /// the server is running. See docs/backup.md for continuous backups.
    Backup {
        /// Destination file; must not exist.
        destination: PathBuf,
    },
    /// Print the OpenAPI document as JSON.
    Openapi,
}

#[derive(Subcommand)]
enum KeysCommand {
    /// Create a key and print its token (shown only once).
    Create {
        #[arg(long)]
        name: String,
        #[arg(long, value_enum, default_value = "write")]
        scope: Scope,
    },
    /// List keys.
    List,
    /// Revoke a key by id.
    Revoke { id: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let default_filter = "rolo=info,tower_http=info";
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| default_filter.into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Openapi => println!("{}", rolo::openapi().to_pretty_json()?),
        Command::Serve {
            addr,
            allowed_hosts,
            allow_private_webhooks,
        } => serve(&cli.db, addr, allowed_hosts, allow_private_webhooks).await?,
        Command::Keys(command) => keys(&cli.db, command).await?,
        Command::Backup { destination } => backup(&cli.db, &destination).await?,
    }
    Ok(())
}

async fn serve(
    path: &Path,
    addr: SocketAddr,
    allowed_hosts: Vec<String>,
    allow_private_webhooks: bool,
) -> anyhow::Result<()> {
    let db = open(path).await?;
    let shutdown = CancellationToken::new();
    let worker_config = WorkerConfig {
        allow_private_destinations: allow_private_webhooks,
        ..WorkerConfig::default()
    };
    let worker = delivery::spawn(db.clone(), worker_config, shutdown.clone())?;

    let app = rolo::app(db, AppConfig { allowed_hosts });
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!("rolo listening on http://{addr} (docs at /docs, MCP at /mcp)");
    let server_shutdown = shutdown.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            server_shutdown.cancel();
        })
        .await?;
    shutdown.cancel();
    worker.await.context("webhook worker panicked")?;
    Ok(())
}

async fn keys(path: &Path, command: KeysCommand) -> anyhow::Result<()> {
    let db = open(path).await?;
    match command {
        KeysCommand::Create { name, scope } => {
            let (key, token) = api_keys::create(&db, &name, scope).await?;
            eprintln!(
                "Created {scope} key {} ({}). Store the token now; it won't be shown again:",
                key.id, key.name
            );
            println!("{token}");
        }
        KeysCommand::List => {
            for key in api_keys::list(&db).await? {
                let status = match key.revoked_at {
                    Some(at) => format!("revoked {}", at.format("%Y-%m-%d")),
                    None => "active".into(),
                };
                let last_used = match key.last_used_at {
                    Some(at) => at.format("%Y-%m-%d %H:%M").to_string(),
                    None => "never".into(),
                };
                println!(
                    "{}  {}…  {:<5}  {:<14}  last used {}  {}",
                    key.id, key.prefix, key.scope, status, last_used, key.name
                );
            }
        }
        KeysCommand::Revoke { id } => {
            api_keys::revoke(&db, &id).await?;
            eprintln!("Revoked {id}");
        }
    }
    Ok(())
}

async fn backup(path: &Path, destination: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(path.exists(), "database {} does not exist", path.display());
    let db = open(path).await?;
    db.backup_to(destination).await?;
    let size = std::fs::metadata(destination)?.len();
    eprintln!(
        "Backed up {} to {} ({size} bytes)",
        path.display(),
        destination.display()
    );
    Ok(())
}

async fn open(path: &Path) -> anyhow::Result<Db> {
    Db::open(path)
        .await
        .with_context(|| format!("opening database {}", path.display()))
}
