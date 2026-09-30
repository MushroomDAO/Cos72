//! The append-only points ledger + its balance replay (docs/agent/spec.md
//! 「数据模型」`points_ledger`, 「入账」). This is the ONLY module that ever
//! inserts into `points_ledger` — the migration's own `BEFORE UPDATE`/
//! `BEFORE DELETE` triggers are the backstop
//! (docs/agent/architecture.md 不可动摇的边界 #5), this module is the
//! reason those statements never appear anywhere else in `src/`
//! (tests/structure.rs `ledger_never_updated_or_deleted_in_src`).
//!
//! [`credit_award`] is the one function that turns an `awaiting` award with
//! a kernel-approved `approval_id` into: a ledger row, `awards.state =
//! 'credited'`, `tasks.status = 'completed'`, and one `outbox` row — all in
//! the SAME `BEGIN IMMEDIATE` transaction (docs/agent/spec.md「入账」).

use sqlx::sqlite::SqliteConnection;
use sqlx::{Row, SqlitePool};

use crate::store::Result;

/// One balance row — `GET /points` (docs/agent/spec.md「REST 路由」).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Balance {
    pub member: String,
    pub balance: i64,
}

/// docs/agent/spec.md「REST 路由」`GET /points`: `{balances: [...], entries:
/// n}` — balances are the replay (`SUM(delta) GROUP BY member`), `entries`
/// is the total row count of `points_ledger` (docs/agent/tasks.md T1.3.1 验
/// 收命令 #3 `balance_equals_replay_of_ledger`/`restart_rebuilds_same_
/// balances`: there is no other source of truth this could disagree with).
///
/// # Errors
/// Any sqlx error.
pub async fn all_balances(pool: &SqlitePool) -> Result<(Vec<Balance>, i64)> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT member, SUM(delta) AS balance FROM points_ledger GROUP BY member ORDER BY member",
    )
    .fetch_all(pool)
    .await?;
    let entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM points_ledger")
        .fetch_one(pool)
        .await?;
    let balances = rows
        .into_iter()
        .map(|(member, balance)| Balance { member, balance })
        .collect();
    Ok((balances, entries))
}

/// docs/agent/spec.md「REST 路由」`GET /points/{member}`: balance (0 if no
/// rows) plus every ledger entry for that member, oldest first.
///
/// # Errors
/// Any sqlx error.
pub async fn balance_for_member(pool: &SqlitePool, member: &str) -> Result<i64> {
    let balance: Option<i64> =
        sqlx::query_scalar("SELECT SUM(delta) FROM points_ledger WHERE member = ?")
            .bind(member)
            .fetch_one(pool)
            .await?;
    Ok(balance.unwrap_or(0))
}

/// One `points_ledger` row, as read back (docs/agent/spec.md「REST 路由」
/// `GET /points/{member}`: "entries: [...]").
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LedgerEntry {
    pub seq: i64,
    pub award_id: String,
    pub member: String,
    pub delta: i64,
    pub task_id: String,
    pub approval_id: String,
    pub created_at: String,
}

/// # Errors
/// Any sqlx error.
pub async fn entries_for_member(pool: &SqlitePool, member: &str) -> Result<Vec<LedgerEntry>> {
    let rows = sqlx::query_as::<_, LedgerEntry>(
        "SELECT seq, award_id, member, delta, task_id, approval_id, created_at FROM \
         points_ledger WHERE member = ? ORDER BY seq ASC",
    )
    .bind(member)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// docs/agent/spec.md「入账」outcome — [`credit_award`] never errors on the
/// "already handled" cases, since those are ordinary outcomes of a poller
/// re-scanning a row it (or a previous generation) already finished with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreditOutcome {
    /// This call performed the credit: ledger row inserted, award
    /// `credited`, task `completed`, outbox row enqueued.
    Credited,
    /// The award was no longer `awaiting` (already `credited` by a previous
    /// poll tick, `denied`, or `expired`) — nothing was written. Also covers
    /// the `award_id UNIQUE` conflict on `points_ledger` (docs/agent/spec.md
    /// 「入账」: "award_id UNIQUE 冲突 = 已入账，视为成功、不报错").
    AlreadyHandled,
    /// No award row exists for this id at all — an orphan approval the
    /// kernel reports `approved` for, but Cos72 never recorded
    /// (docs/agent/spec.md「错误处理/幂等」: "孤儿被批准也不入账 → 只会少发").
    UnknownAward,
}

/// docs/agent/spec.md「入账」`approved` case, run inside one `BEGIN
/// IMMEDIATE` transaction: re-confirm the award (by `approval_id`) is still
/// `awaiting`, insert the ledger row, credit the award, complete the task,
/// enqueue the `memory.remember` outbox row. The three-way guard
/// (docs/agent/architecture.md 不可动摇的边界 #2 — "自己记下的 approval_id" /
/// "内核 approved" / "award_id 未入账") is: the caller only ever calls this
/// for an `approval_id` FOUND in `awards` (query is `WHERE approval_id = ?
/// AND state = 'awaiting'`, so an orphan approval id that is not in
/// `awards` at all returns [`CreditOutcome::UnknownAward`] without writing
/// anything), the caller only calls this after the kernel's own `status`
/// answered `approved`, and the ledger insert's `award_id UNIQUE`
/// constraint is the final backstop against a double-credit even if this
/// function were somehow invoked twice concurrently for the same award.
///
/// # Errors
/// Any sqlx error (the transaction is rolled back before the error is
/// returned).
pub async fn credit_award(
    pool: &SqlitePool,
    approval_id: &str,
    now: &str,
) -> Result<CreditOutcome> {
    let mut conn = pool.acquire().await?;
    sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
    let outcome = credit_award_locked(&mut conn, approval_id, now).await;
    match &outcome {
        Ok(CreditOutcome::Credited) => {
            sqlx::query("COMMIT").execute(&mut *conn).await?;
        }
        _ => {
            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
        }
    }
    outcome
}

async fn credit_award_locked(
    conn: &mut SqliteConnection,
    approval_id: &str,
    now: &str,
) -> Result<CreditOutcome> {
    let row = sqlx::query(
        "SELECT award_id, task_id, member, points, state FROM awards WHERE approval_id = ?",
    )
    .bind(approval_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(CreditOutcome::UnknownAward);
    };
    let award_id: String = row.try_get("award_id")?;
    let task_id: String = row.try_get("task_id")?;
    let member: String = row.try_get("member")?;
    let points: i64 = row.try_get("points")?;
    let state: String = row.try_get("state")?;
    if state != "awaiting" {
        return Ok(CreditOutcome::AlreadyHandled);
    }

    let inserted = sqlx::query(
        "INSERT INTO points_ledger (award_id, member, delta, task_id, approval_id, created_at) \
         VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(award_id) DO NOTHING",
    )
    .bind(&award_id)
    .bind(&member)
    .bind(points)
    .bind(&task_id)
    .bind(approval_id)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    if inserted.rows_affected() == 0 {
        // `award_id UNIQUE` already had a row — a previous tick (or, in
        // theory, a racing caller) already credited this exact award.
        return Ok(CreditOutcome::AlreadyHandled);
    }

    sqlx::query("UPDATE awards SET state = 'credited', decided_at = ? WHERE award_id = ?")
        .bind(now)
        .bind(&award_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("UPDATE tasks SET status = 'completed', updated_at = ? WHERE task_id = ?")
        .bind(now)
        .bind(&task_id)
        .execute(&mut *conn)
        .await?;

    // docs/agent/spec.md「记忆泵」: the outbox payload carries `title` too
    // (for a human-readable summary) — read from `tasks` since `awards`
    // itself never stores it.
    let title: String = sqlx::query_scalar("SELECT title FROM tasks WHERE task_id = ?")
        .bind(&task_id)
        .fetch_one(&mut *conn)
        .await?;

    let dedup_key = format!("cos72:task:{task_id}:completed");
    let payload = serde_json::json!({
        "task_id": task_id,
        "title": title,
        "member": member,
        "points": points,
        "award_id": award_id,
        "approval_id": approval_id,
        "completed_at": now,
    })
    .to_string();
    sqlx::query(
        "INSERT INTO outbox (kind, dedup_key, payload, state, attempts, next_attempt_at) \
         VALUES ('memory.remember', ?, ?, 'pending', 0, ?) ON CONFLICT(dedup_key) DO NOTHING",
    )
    .bind(&dedup_key)
    .bind(&payload)
    .bind(now)
    .execute(&mut *conn)
    .await?;

    Ok(CreditOutcome::Credited)
}

/// docs/agent/spec.md「入账」`denied` case: same-transaction `awards.state
/// = 'denied'` + `tasks.status = 'claimed'`. Same "already handled" posture
/// as [`credit_award`] — a poller re-scanning a row it (or a previous
/// generation) already denied does nothing, not an error.
///
/// # Errors
/// Any sqlx error.
pub async fn deny_award(pool: &SqlitePool, approval_id: &str, now: &str) -> Result<CreditOutcome> {
    let mut conn = pool.acquire().await?;
    sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
    let outcome = deny_or_expire_locked(&mut conn, approval_id, now, "denied", "claimed").await;
    finish_txn(&mut conn, &outcome).await?;
    outcome
}

/// docs/agent/spec.md「入账」`timed_out` case: same-transaction
/// `awards.state = 'expired'` — the task's own `status` is left untouched
/// (spec.md: "仍是 submitted"), so `next_status` is `None` here (see
/// [`deny_or_expire_locked`]'s `task_status` parameter).
///
/// # Errors
/// Any sqlx error.
pub async fn expire_award(
    pool: &SqlitePool,
    approval_id: &str,
    now: &str,
) -> Result<CreditOutcome> {
    let mut conn = pool.acquire().await?;
    sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
    let outcome = deny_or_expire_locked(&mut conn, approval_id, now, "expired", "submitted").await;
    finish_txn(&mut conn, &outcome).await?;
    outcome
}

async fn finish_txn(conn: &mut SqliteConnection, outcome: &Result<CreditOutcome>) -> Result<()> {
    match outcome {
        Ok(CreditOutcome::Credited) => {
            sqlx::query("COMMIT").execute(&mut *conn).await?;
        }
        _ => {
            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
        }
    }
    Ok(())
}

/// Shared body of [`deny_award`]/[`expire_award`]: both only ever flip
/// `awards.state` (never touch `points_ledger`) and, for `denied`, also
/// flip `tasks.status` back to `claimed`; `expired` leaves the task's own
/// status untouched (still `submitted` — spec.md's state diagram). Reuses
/// [`CreditOutcome::Credited`] as "this call did the write" purely to share
/// [`finish_txn`]'s commit/rollback decision with [`credit_award`] — the
/// award's own `state` column (not this return value) is what callers
/// actually branch on when they need to know WHICH terminal state a row
/// landed in.
async fn deny_or_expire_locked(
    conn: &mut SqliteConnection,
    approval_id: &str,
    now: &str,
    new_state: &str,
    task_status_if_denied: &str,
) -> Result<CreditOutcome> {
    let row = sqlx::query("SELECT award_id, task_id, state FROM awards WHERE approval_id = ?")
        .bind(approval_id)
        .fetch_optional(&mut *conn)
        .await?;
    let Some(row) = row else {
        return Ok(CreditOutcome::UnknownAward);
    };
    let award_id: String = row.try_get("award_id")?;
    let task_id: String = row.try_get("task_id")?;
    let state: String = row.try_get("state")?;
    if state != "awaiting" {
        return Ok(CreditOutcome::AlreadyHandled);
    }

    sqlx::query("UPDATE awards SET state = ?, decided_at = ? WHERE award_id = ?")
        .bind(new_state)
        .bind(now)
        .bind(&award_id)
        .execute(&mut *conn)
        .await?;
    if new_state == "denied" {
        sqlx::query("UPDATE tasks SET status = ?, updated_at = ? WHERE task_id = ?")
            .bind(task_status_if_denied)
            .bind(now)
            .bind(&task_id)
            .execute(&mut *conn)
            .await?;
    }
    Ok(CreditOutcome::Credited)
}
