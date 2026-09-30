//! Cos72 persistence — sqlx over its OWN `cos72.db` (never the kernel's
//! `agent24.db`, docs/agent/architecture.md 不可动摇的边界 #1: "SQLite 是
//! 真相"). T1.1.1 only opens the pool and runs the full T1.1.1-scope
//! migration (docs/agent/spec.md「数据模型」, all four tables); the
//! repositories (`tasks.rs` / `ledger.rs`) are T1.2.1/T1.3.1.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::Path;
use std::str::FromStr;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Owns the connection pool. `Clone` is cheap (an `Arc`-backed
/// `sqlx::SqlitePool`, same pattern Sin90's `Sin90Store` uses).
#[derive(Clone)]
pub struct Cos72Store {
    // Not read outside this module's own tests yet — T1.1.1 has no
    // repository code (docs/agent/tasks.md「明确不做」: "任务/奖励/账本的
    // 业务代码"). T1.2.1's `store::tasks`/T1.3.1's `store::ledger` become the
    // real callers of `pool()` below.
    #[allow(dead_code)]
    pool: SqlitePool,
}

impl Cos72Store {
    /// Open (creating if needed) `cos72.db` under `path`'s parent and run
    /// every migration. docs/agent/spec.md「数据模型」connection parameters:
    /// `journal_mode=WAL`, `foreign_keys=ON`, `busy_timeout=5000`.
    ///
    /// # Errors
    /// The parent directory cannot be created, the pool cannot be opened, or
    /// a migration fails.
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_millis(5000))
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    /// A per-instance named shared-cache in-memory database (same reasoning
    /// as Sin90's `Sin90Store::open_memory`: `min_connections(1)` +
    /// `idle_timeout(None)` + `max_lifetime(None)` so the reaper never closes
    /// the only connection holding the in-memory database's storage alive) —
    /// a unique name per call so concurrent tests never collide.
    ///
    /// # Errors
    /// The pool cannot be opened, or a migration fails.
    pub async fn open_memory() -> Result<Self> {
        let uri = format!(
            "sqlite:file:cos72-{}?mode=memory&cache=shared",
            ulid::Ulid::new()
        );
        let options = SqliteConnectOptions::from_str(&uri)?
            .busy_timeout(std::time::Duration::from_millis(5000))
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    /// The underlying pool — `pub(crate)` so a future business repository
    /// (T1.2.1's `store::tasks`, T1.3.1's `store::ledger`) is the real
    /// caller; this module's own tests are the only caller today.
    /// `tests/migrations.rs` (a separate integration-test crate) opens its
    /// own pool directly against a temp file instead of reaching into this
    /// one.
    #[allow(dead_code)]
    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The schema opens cleanly and every T1.1.1-scope table exists (a
    /// smoke test — `tests/migrations.rs` is the real constraint coverage,
    /// docs/agent/tasks.md T1.1.1 验收命令 #2).
    #[tokio::test]
    async fn open_memory_runs_all_migrations() {
        let store = Cos72Store::open_memory().await.unwrap();
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' \
             AND name NOT LIKE '_sqlx_%' ORDER BY name",
        )
        .fetch_all(store.pool())
        .await
        .unwrap();
        assert_eq!(tables, vec!["awards", "outbox", "points_ledger", "tasks"]);
    }
}
