use std::{path::Path, str::FromStr, time::Duration};

use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

/// SQLite allows one writer at a time, so writes go through a single-connection
/// pool (queued in-process instead of contending on the file lock) and reads use
/// a separate read-only pool.
#[derive(Clone)]
pub struct Db {
    pub reader: SqlitePool,
    pub writer: SqlitePool,
}

impl Db {
    pub async fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
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
}
