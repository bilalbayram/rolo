use std::{net::SocketAddr, path::PathBuf};

use anyhow::Context;
use clap::{Parser, Subcommand};
use rolo::{
    AppConfig,
    db::Db,
    service::api_keys::{self, Scope},
};
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
    /// Run the HTTP server (REST API at /v1, MCP at /mcp, docs at /docs).
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
    },
    /// Manage API keys.
    #[command(subcommand)]
    Keys(KeysCommand),
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
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rolo=info,tower_http=info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Openapi => {
            println!("{}", rolo::openapi().to_pretty_json()?);
        }
        Command::Serve {
            addr,
            allowed_hosts,
        } => {
            let db = open(&cli.db).await?;
            let app = rolo::app(db, AppConfig { allowed_hosts });
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .with_context(|| format!("binding {addr}"))?;
            tracing::info!("rolo listening on http://{addr} (docs at /docs, MCP at /mcp)");
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
        }
        Command::Keys(KeysCommand::Create { name, scope }) => {
            let db = open(&cli.db).await?;
            let (key, token) = api_keys::create(&db, &name, scope).await?;
            eprintln!(
                "Created {} key {} ({}). Store the token now; it won't be shown again:",
                key_scope(scope),
                key.id,
                key.name
            );
            println!("{token}");
        }
        Command::Keys(KeysCommand::List) => {
            let db = open(&cli.db).await?;
            for key in api_keys::list(&db).await? {
                let status = match key.revoked_at {
                    Some(at) => format!("revoked {}", at.format("%Y-%m-%d")),
                    None => "active".into(),
                };
                let last_used = key
                    .last_used_at
                    .map_or("never".into(), |t| t.format("%Y-%m-%d %H:%M").to_string());
                println!(
                    "{}  {}…  {:<5}  {:<14}  last used {}  {}",
                    key.id,
                    key.prefix,
                    key_scope(key.scope),
                    status,
                    last_used,
                    key.name
                );
            }
        }
        Command::Keys(KeysCommand::Revoke { id }) => {
            let db = open(&cli.db).await?;
            api_keys::revoke(&db, &id).await?;
            eprintln!("Revoked {id}");
        }
    }
    Ok(())
}

async fn open(path: &PathBuf) -> anyhow::Result<Db> {
    Db::open(path)
        .await
        .with_context(|| format!("opening database {}", path.display()))
}

fn key_scope(scope: Scope) -> &'static str {
    match scope {
        Scope::Read => "read",
        Scope::Write => "write",
    }
}
