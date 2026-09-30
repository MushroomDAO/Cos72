//! The `outbox` repository (docs/agent/spec.md「数据模型」`outbox`) —
//! `store::ledger::credit_award` is the only writer of NEW rows (one
//! `INSERT ... ON CONFLICT(dedup_key) DO NOTHING` per completed task, in the
//! same transaction as the credit); this module only ever reads due rows and
//! advances their own `state`/`attempts`/`next_attempt_at`/`last_error`/
//! `result_ref` columns — `workers::memory_pump` is its only caller.

use sqlx::SqlitePool;

use crate::store::Result;

/// One `outbox` row, as read back.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OutboxRow {
    pub id: i64,
    pub kind: String,
    pub dedup_key: String,
    pub payload: String,
    pub state: String,
    pub attempts: i64,
    pub next_attempt_at: String,
    pub last_error: Option<String>,
    pub result_ref: Option<String>,
}

/// docs/agent/spec.md「记忆泵」: "取 `state='pending' AND next_attempt_at <=
/// now` 的 outbox 行" — oldest (lowest `id`) first.
///
/// # Errors
/// Any sqlx error.
pub async fn fetch_due_pending(pool: &SqlitePool, now: &str) -> Result<Vec<OutboxRow>> {
    let rows = sqlx::query_as::<_, OutboxRow>(
        "SELECT id, kind, dedup_key, payload, state, attempts, next_attempt_at, last_error, \
         result_ref FROM outbox WHERE state = 'pending' AND next_attempt_at <= ? ORDER BY id ASC",
    )
    .bind(now)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// docs/agent/spec.md「记忆泵」: `Created{id}` / `Found{id}` → `done`,
/// `result_ref=id`.
///
/// # Errors
/// Any sqlx error.
pub async fn mark_done(pool: &SqlitePool, id: i64, result_ref: &str) -> Result<()> {
    sqlx::query("UPDATE outbox SET state = 'done', result_ref = ?, last_error = NULL WHERE id = ?")
        .bind(result_ref)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// docs/agent/spec.md「记忆泵」: a permanent (`is_permanent()`) `ClientError`
/// → `dead` + `last_error`.
///
/// # Errors
/// Any sqlx error.
pub async fn mark_dead(pool: &SqlitePool, id: i64, last_error: &str) -> Result<()> {
    sqlx::query("UPDATE outbox SET state = 'dead', last_error = ? WHERE id = ?")
        .bind(last_error)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// docs/agent/spec.md「记忆泵」: a retryable failure (or `Inconclusive`, "不
/// 当作不存在") → stays `pending`, `attempts += 1`, `next_attempt_at` pushed
/// out by the caller's own backoff schedule.
///
/// # Errors
/// Any sqlx error.
pub async fn bump_retry(
    pool: &SqlitePool,
    id: i64,
    next_attempt_at: &str,
    last_error: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "UPDATE outbox SET attempts = attempts + 1, next_attempt_at = ?, last_error = ? WHERE id \
         = ?",
    )
    .bind(next_attempt_at)
    .bind(last_error)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Cos72Store;

    async fn insert_pending(pool: &SqlitePool, dedup_key: &str, next_attempt_at: &str) -> i64 {
        sqlx::query(
            "INSERT INTO outbox (kind, dedup_key, payload, state, attempts, next_attempt_at) \
             VALUES ('memory.remember', ?, '{}', 'pending', 0, ?)",
        )
        .bind(dedup_key)
        .bind(next_attempt_at)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query_scalar("SELECT id FROM outbox WHERE dedup_key = ?")
            .bind(dedup_key)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// `fetch_due_pending` only returns rows whose `next_attempt_at <= now`,
    /// oldest first; 正对照: a not-yet-due row is excluded.
    #[tokio::test]
    async fn fetch_due_pending_filters_by_time_and_orders_oldest_first() {
        let store = Cos72Store::open_memory().await.unwrap();
        insert_pending(store.pool(), "k1", "2026-01-01T00:00:00.000Z").await;
        insert_pending(store.pool(), "k2", "2026-01-01T00:00:01.000Z").await;
        insert_pending(store.pool(), "k3", "2026-06-01T00:00:00.000Z").await; // not due yet

        let due = fetch_due_pending(store.pool(), "2026-01-01T00:00:02.000Z")
            .await
            .unwrap();
        assert_eq!(
            due.iter().map(|r| r.dedup_key.as_str()).collect::<Vec<_>>(),
            vec!["k1", "k2"]
        );
    }

    #[tokio::test]
    async fn mark_done_and_mark_dead_and_bump_retry() {
        let store = Cos72Store::open_memory().await.unwrap();
        let id1 = insert_pending(store.pool(), "k1", "2026-01-01T00:00:00.000Z").await;
        let id2 = insert_pending(store.pool(), "k2", "2026-01-01T00:00:00.000Z").await;
        let id3 = insert_pending(store.pool(), "k3", "2026-01-01T00:00:00.000Z").await;

        mark_done(store.pool(), id1, "osmem:1").await.unwrap();
        mark_dead(store.pool(), id2, "forbidden").await.unwrap();
        bump_retry(
            store.pool(),
            id3,
            "2026-01-01T00:00:05.000Z",
            Some("timeout"),
        )
        .await
        .unwrap();

        let rows: Vec<OutboxRow> = sqlx::query_as(
            "SELECT id, kind, dedup_key, payload, state, attempts, next_attempt_at, last_error, \
             result_ref FROM outbox ORDER BY id",
        )
        .fetch_all(store.pool())
        .await
        .unwrap();
        assert_eq!(rows[0].state, "done");
        assert_eq!(rows[0].result_ref.as_deref(), Some("osmem:1"));
        assert_eq!(rows[1].state, "dead");
        assert_eq!(rows[1].last_error.as_deref(), Some("forbidden"));
        assert_eq!(rows[2].state, "pending", "a retry must stay pending");
        assert_eq!(rows[2].attempts, 1);
        assert_eq!(rows[2].next_attempt_at, "2026-01-01T00:00:05.000Z");

        // 正对照: the row that was NOT touched stays untouched.
        assert_eq!(rows[0].attempts, 0);
    }
}
