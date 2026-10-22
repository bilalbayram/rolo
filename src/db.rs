use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Context;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

/// SQLite allows one writer at a time, so writes go through a single-connection
/// pool (queued in-process instead of contending on the file lock) and reads use
/// a separate read-only pool.
#[derive(Clone)]
pub struct Db {
    pub reader: SqlitePool,
    pub writer: SqlitePool,
}

impl Db {
    /// Open (creating if needed) the database at `path` and run pending migrations.
    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        let base = SqliteConnectOptions::from_str("sqlite://")?
            .filename(path)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));

        let writer = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(base.clone().create_if_missing(true))
            .await?;
        sqlx::migrate!().run(&writer).await?;

        let reader = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(base.read_only(true))
            .await?;

        Ok(Self { reader, writer })
    }

    /// Write a consistent snapshot of the database to `destination`, which must
    /// not exist. Safe while the server is running: it only needs a read transaction.
    pub async fn backup_to(&self, destination: &Path) -> anyhow::Result<()> {
        anyhow::ensure!(
            !destination.exists(),
            "{} already exists",
            destination.display()
        );
        let destination = destination
            .to_str()
            .context("backup path must be valid UTF-8")?;
        sqlx::query("VACUUM INTO ?")
            .bind(destination)
            .execute(&self.reader)
            .await?;
        Ok(())
    }
}
