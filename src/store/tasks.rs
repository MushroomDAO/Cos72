//! `tasks` / `awards` repository (docs/agent/architecture.md「系统骨架」
//! `store/`: "sqlx：迁移、仓储；每次状态变化 BEGIN IMMEDIATE"). This module is
//! the ONLY place that writes `tasks`/`awards` rows for T1.2.1 — `http::tasks`
//! calls these functions and never issues raw SQL of its own.
//!
//! T1.2.1 scope (docs/agent/tasks.md「开发范围」): publish/claim/submit only
//! insert an `awaiting` award row with `approval_id IS NULL` on submit — no
//! `advise`, no ledger, no memory (T1.3.1).

use sqlx::sqlite::SqliteConnection;
use sqlx::{Row, SqlitePool};

use crate::store::{Result, StoreError};

/// A `tasks` row, as read back from SQLite. `status` stays a raw `String`
/// here (not `core::task::TaskStatus`) — this is the boundary between "what
/// the database contains" and "what the domain believes"; callers that need
/// the typed status call `core::task::TaskStatus::parse` themselves. A row
/// failing to parse would mean the migration's own `CHECK` constraint was
/// bypassed, which `store` has no business silently working around.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TaskRow {
    pub task_id: String,
    pub title: String,
    pub description: String,
    pub reward_points: i64,
    pub publisher: String,
    pub claimer: Option<String>,
    pub status: String,
    pub evidence: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// An `awards` row (only the fields T1.2.1's submit path and its own tests
/// need — `approval_id`/`state` are the two columns that matter here since
/// this task only ever inserts `state = 'awaiting'`, `approval_id = NULL`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AwardRow {
    pub award_id: String,
    pub task_id: String,
    pub member: String,
    pub points: i64,
    pub approval_id: Option<String>,
    pub state: String,
}

/// A freshly-validated task to insert (docs/agent/spec.md「REST 路由」
/// `POST /tasks`). Validation (`core::task::is_valid_*`) has already run by
/// the time `http::tasks::publish_task` calls this — this function does not
/// re-validate, it only writes what it is given.
pub struct NewTask<'a> {
    pub task_id: &'a str,
    pub title: &'a str,
    pub description: &'a str,
    pub reward_points: i64,
    pub publisher: &'a str,
    pub created_at: &'a str,
}

/// docs/agent/tasks.md T1.2.1 验收命令 #2: `POST /tasks` → 201 `open`.
///
/// # Errors
/// Any sqlx error (the `reward_points`/`status` `CHECK`s are a backstop —
/// `http::tasks` validates first, so this should not trigger in practice).
pub async fn insert_task(pool: &SqlitePool, task: NewTask<'_>) -> Result<TaskRow> {
    sqlx::query(
        "INSERT INTO tasks (task_id, title, description, reward_points, publisher, claimer, \
         status, evidence, created_at, updated_at) VALUES (?, ?, ?, ?, ?, NULL, 'open', NULL, \
         ?, ?)",
    )
    .bind(task.task_id)
    .bind(task.title)
    .bind(task.description)
    .bind(task.reward_points)
    .bind(task.publisher)
    .bind(task.created_at)
    .bind(task.created_at)
    .execute(pool)
    .await?;

    get_task(pool, task.task_id)
        .await?
        .ok_or_else(|| StoreError::Sqlx(sqlx::Error::RowNotFound))
}

/// docs/agent/spec.md「REST 路由」`GET /tasks/{id}`.
///
/// # Errors
/// Any sqlx error.
pub async fn get_task(pool: &SqlitePool, task_id: &str) -> Result<Option<TaskRow>> {
    let row = sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE task_id = ?")
        .bind(task_id)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

/// docs/agent/spec.md「REST 路由」`GET /tasks?status=`: newest first, capped
/// at 200 ⚖️. `status` filters to that one status when given.
///
/// # Errors
/// Any sqlx error.
pub async fn list_tasks(pool: &SqlitePool, status: Option<&str>) -> Result<Vec<TaskRow>> {
    let rows = match status {
        Some(status) => {
            sqlx::query_as::<_, TaskRow>(
                "SELECT * FROM tasks WHERE status = ? ORDER BY created_at DESC LIMIT 200",
            )
            .bind(status)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks ORDER BY created_at DESC LIMIT 200")
                .fetch_all(pool)
                .await?
        }
    };
    Ok(rows)
}

/// The most recently created award for a task (docs/agent/spec.md「REST 路
/// 由」`GET /tasks/{id}`: "含当前 award 摘要") — `ORDER BY rowid DESC` since
/// `award_id` (a ULID) sorts lexicographically the same as creation order,
/// but `rowid` is the one column guaranteed monotonic regardless of that.
///
/// # Errors
/// Any sqlx error.
pub async fn latest_award_for_task(pool: &SqlitePool, task_id: &str) -> Result<Option<AwardRow>> {
    let row = sqlx::query_as::<_, AwardRow>(
        "SELECT award_id, task_id, member, points, approval_id, state FROM awards \
         WHERE task_id = ? ORDER BY rowid DESC LIMIT 1",
    )
    .bind(task_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Outcome of a claim attempt — deliberately NOT a `Result` with an error
/// variant for the "already claimed" / "not found" cases: those are
/// ordinary, expected outcomes of a race (docs/agent/tasks.md T1.2.1 验收命
/// 令 #2 `concurrent_claims_exactly_one_wins`), not failures of the store
/// itself.
pub enum ClaimOutcome {
    // Boxed (clippy::large_enum_variant): `TaskRow` is ~224 bytes of owned
    // `String`s next to zero-sized siblings.
    Claimed(Box<TaskRow>),
    NotFound,
    /// The task exists but was not `open` (already claimed, submitted, or
    /// completed) — 409 `invalid_transition` at the HTTP layer.
    InvalidTransition,
}

/// docs/agent/spec.md「状态机」`open → claimed`. The single `UPDATE ... WHERE
/// status = 'open'` IS the compare-and-swap (docs/agent/tasks.md T1.2.1「并
/// 发领取用 SQL 条件更新（CAS）保证恰好一个成功」): SQLite serializes writers
/// at the statement level, and the pool's `busy_timeout=5000` makes
/// concurrent losers wait for the winner's write rather than error out, so
/// exactly one of N concurrent callers sees `rows_affected() == 1`.
///
/// # Errors
/// Any sqlx error.
pub async fn claim_task(
    pool: &SqlitePool,
    task_id: &str,
    member: &str,
    now: &str,
) -> Result<ClaimOutcome> {
    let result = sqlx::query(
        "UPDATE tasks SET status = 'claimed', claimer = ?, updated_at = ? WHERE task_id = ? \
         AND status = 'open'",
    )
    .bind(member)
    .bind(now)
    .bind(task_id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 1 {
        let row = get_task(pool, task_id)
            .await?
            .ok_or_else(|| StoreError::Sqlx(sqlx::Error::RowNotFound))?;
        return Ok(ClaimOutcome::Claimed(Box::new(row)));
    }

    // The CAS matched 0 rows: either the task does not exist, or it exists
    // but was not `open`. This second SELECT only runs on the (comparatively
    // rare) losing/error path, never on the winning path above.
    match get_task(pool, task_id).await? {
        Some(_) => Ok(ClaimOutcome::InvalidTransition),
        None => Ok(ClaimOutcome::NotFound),
    }
}

/// Outcome of a submit attempt (docs/agent/spec.md「submit 的精确步骤」第 2
/// 步 — this is the store's view of that step; the HTTP handler decides
/// whether/how to call `advise` from here).
pub enum SubmitOutcome {
    /// This call needs to (re-)advise for `award_id` — either because
    /// `claimed → submitted` just happened (`emit_task_submitted = true`),
    /// or because the task was already `submitted` and its current
    /// `awaiting` award has `approval_id IS NULL` (孤儿补偿: a previous
    /// advise never got its id written back) or no `awaiting` award exists
    /// at all (the previous one `expired`) — both reuse/create scenarios
    /// need a fresh `advise`, but must NOT re-emit `task.submitted` (the
    /// task did not just transition).
    // Boxed for the same reason as `ClaimOutcome::Claimed`.
    NeedsAdvise {
        task: Box<TaskRow>,
        award_id: String,
        emit_task_submitted: bool,
    },
    /// The task was already `submitted` and its current `awaiting` award
    /// already has an `approval_id` — idempotent re-entry, spec.md「submit
    /// 的精确步骤」第 2 步: "不再 advise，直接 200 返回现状".
    AlreadyAdvised {
        task: Box<TaskRow>,
        award_id: String,
        approval_id: String,
    },
    NotFound,
    /// The task exists but is neither `claimed` nor `submitted` (open or
    /// completed) — 409.
    InvalidTransition,
    /// The task is `claimed`/`submitted`, but by someone else — 403
    /// `not_claimer`.
    NotClaimer,
}

/// docs/agent/spec.md「状态机」`claimed → submitted`, plus the `submitted →
/// submitted` re-entry cases (「submit 的精确步骤」第 2 步) — the
/// read-decide-write happens inside one `BEGIN IMMEDIATE` transaction
/// (docs/agent/architecture.md 核心判断 4 / 不可动摇的边界: transitions are
/// one SQLite transaction), so no concurrent submit/claim/poller-credit can
/// observe or produce a half-applied state. `BEGIN IMMEDIATE` (not the
/// default `DEFERRED`) takes the write lock up front, before the `SELECT`
/// that decides the outcome, so the decision and the write it leads to can
/// never straddle a lock handoff to another writer.
///
/// `new_award_id` is only used when this call ends up inserting a fresh
/// `awards` row (the `claimed → submitted` transition, or a `submitted`
/// re-entry with no `awaiting` row left because the previous one expired) —
/// the `submitted`-with-an-existing-`awaiting`-row cases reuse that row's
/// own `award_id` instead and `new_award_id` goes unused.
///
/// # Errors
/// Any sqlx error (the transaction is rolled back before the error is
/// returned).
pub async fn submit_task(
    pool: &SqlitePool,
    task_id: &str,
    member: &str,
    evidence: Option<&str>,
    now: &str,
    new_award_id: &str,
) -> Result<SubmitOutcome> {
    let mut conn = pool.acquire().await?;
    sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;

    // Run the read-decide-write as a closure so every exit path (including
    // an sqlx error from any statement) goes through the same ROLLBACK
    // before propagating, instead of every early return above duplicating it.
    // `&mut conn` (a `PoolConnection<Sqlite>`) deref-coerces to `&mut
    // SqliteConnection` at this call site.
    let outcome = submit_task_locked(&mut conn, task_id, member, evidence, now, new_award_id).await;

    match &outcome {
        Ok(SubmitOutcome::NeedsAdvise { .. } | SubmitOutcome::AlreadyAdvised { .. }) => {
            sqlx::query("COMMIT").execute(&mut *conn).await?;
        }
        _ => {
            // Both the "no-op" outcomes (NotFound/InvalidTransition/NotClaimer)
            // and any sqlx error roll back — nothing was written either way.
            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
        }
    }
    outcome
}

/// The part of [`submit_task`] that runs inside the already-open `BEGIN
/// IMMEDIATE` transaction — split out so `submit_task` has exactly one
/// COMMIT/ROLLBACK decision point instead of one per early return.
async fn submit_task_locked(
    conn: &mut SqliteConnection,
    task_id: &str,
    member: &str,
    evidence: Option<&str>,
    now: &str,
    new_award_id: &str,
) -> Result<SubmitOutcome> {
    let row = sqlx::query("SELECT status, claimer, reward_points FROM tasks WHERE task_id = ?")
        .bind(task_id)
        .fetch_optional(&mut *conn)
        .await?;

    let Some(row) = row else {
        return Ok(SubmitOutcome::NotFound);
    };
    let status: String = row.try_get("status")?;
    let claimer: Option<String> = row.try_get("claimer")?;
    let reward_points: i64 = row.try_get("reward_points")?;

    if status != "claimed" && status != "submitted" {
        return Ok(SubmitOutcome::InvalidTransition);
    }
    if claimer.as_deref() != Some(member) {
        return Ok(SubmitOutcome::NotClaimer);
    }

    if status == "claimed" {
        sqlx::query(
            "UPDATE tasks SET status = 'submitted', evidence = ?, updated_at = ? WHERE task_id \
             = ?",
        )
        .bind(evidence)
        .bind(now)
        .bind(task_id)
        .execute(&mut *conn)
        .await?;

        insert_awaiting_award(
            &mut *conn,
            new_award_id,
            task_id,
            member,
            reward_points,
            now,
        )
        .await?;

        let task = refetch_task(&mut *conn, task_id).await?;
        return Ok(SubmitOutcome::NeedsAdvise {
            task: Box::new(task),
            award_id: new_award_id.to_owned(),
            emit_task_submitted: true,
        });
    }

    // `status == "submitted"`: spec.md「submit 的精确步骤」第 2 步, second
    // bullet. The partial unique index `idx_awards_one_awaiting_per_task`
    // guarantees at most one `awaiting` row for this task.
    let awaiting = sqlx::query(
        "SELECT award_id, approval_id FROM awards WHERE task_id = ? AND state = 'awaiting'",
    )
    .bind(task_id)
    .fetch_optional(&mut *conn)
    .await?;

    match awaiting {
        Some(row) => {
            let award_id: String = row.try_get("award_id")?;
            let approval_id: Option<String> = row.try_get("approval_id")?;
            let task = refetch_task(&mut *conn, task_id).await?;
            match approval_id {
                None => Ok(SubmitOutcome::NeedsAdvise {
                    task: Box::new(task),
                    award_id,
                    emit_task_submitted: false,
                }),
                Some(approval_id) => Ok(SubmitOutcome::AlreadyAdvised {
                    task: Box::new(task),
                    award_id,
                    approval_id,
                }),
            }
        }
        None => {
            // The previous award for this task `expired` (or was `denied`,
            // though `denied` sends the task back to `claimed` so it would
            // not be `submitted` here) — insert a fresh one.
            insert_awaiting_award(
                &mut *conn,
                new_award_id,
                task_id,
                member,
                reward_points,
                now,
            )
            .await?;
            let task = refetch_task(&mut *conn, task_id).await?;
            Ok(SubmitOutcome::NeedsAdvise {
                task: Box::new(task),
                award_id: new_award_id.to_owned(),
                emit_task_submitted: false,
            })
        }
    }
}

async fn insert_awaiting_award(
    conn: &mut SqliteConnection,
    award_id: &str,
    task_id: &str,
    member: &str,
    points: i64,
    now: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO awards (award_id, task_id, member, points, approval_id, state, created_at) \
         VALUES (?, ?, ?, ?, NULL, 'awaiting', ?)",
    )
    .bind(award_id)
    .bind(task_id)
    .bind(member)
    .bind(points)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn refetch_task(conn: &mut SqliteConnection, task_id: &str) -> Result<TaskRow> {
    let task = sqlx::query_as::<_, TaskRow>("SELECT * FROM tasks WHERE task_id = ?")
        .bind(task_id)
        .fetch_one(&mut *conn)
        .await?;
    Ok(task)
}

/// docs/agent/spec.md「submit 的精确步骤」第 4 步: `UPDATE awards SET
/// approval_id = ? WHERE award_id = ? AND state = 'awaiting' AND
/// approval_id IS NULL` — a plain CAS via `rows_affected()`, no
/// transaction of its own needed (a single `UPDATE` is already atomic).
///
/// # Errors
/// Any sqlx error.
pub enum CasApprovalOutcome {
    /// This call's `approval_id` was written — it is now the award's
    /// recorded approval, and this poll thread's future `status` calls will
    /// use it.
    Written,
    /// 0 rows matched: the award moved on before this write landed.
    /// `existing_approval_id` is `Some` when another writer's CAS already
    /// occupied this row (docs/agent/spec.md「submit 的精确步骤」第 4 步:
    /// "本次 advise 产生的审批成为孤儿" — this call's own `approval_id` is
    /// simply never recorded anywhere and is therefore never credited);
    /// `None` in the (very rare) case the award left `awaiting` entirely
    /// between this call's `advise` and its CAS write (e.g. `expired` via
    /// TTL) — that award's approval never gets an id recorded either, same
    /// orphan outcome.
    LostRace {
        existing_approval_id: Option<String>,
    },
}

pub async fn cas_write_approval_id(
    pool: &SqlitePool,
    award_id: &str,
    approval_id: &str,
) -> Result<CasApprovalOutcome> {
    let result = sqlx::query(
        "UPDATE awards SET approval_id = ? WHERE award_id = ? AND state = 'awaiting' AND \
         approval_id IS NULL",
    )
    .bind(approval_id)
    .bind(award_id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 1 {
        return Ok(CasApprovalOutcome::Written);
    }
    let existing: Option<Option<String>> =
        sqlx::query_scalar("SELECT approval_id FROM awards WHERE award_id = ?")
            .bind(award_id)
            .fetch_optional(pool)
            .await?;
    Ok(CasApprovalOutcome::LostRace {
        existing_approval_id: existing.flatten(),
    })
}

/// docs/agent/spec.md「入账」: every `awaiting` award with an `approval_id`
/// already recorded — the award_poller's own work queue. Order is
/// unspecified but stable-ish (`rowid`) so tests can reason about it.
///
/// # Errors
/// Any sqlx error.
pub async fn list_awards_pending_poll(pool: &SqlitePool) -> Result<Vec<AwardRow>> {
    let rows = sqlx::query_as::<_, AwardRow>(
        "SELECT award_id, task_id, member, points, approval_id, state FROM awards WHERE state = \
         'awaiting' AND approval_id IS NOT NULL ORDER BY rowid ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// docs/agent/spec.md「入账」错误处理: a retryable/unclassified `status`
/// error just records `last_poll_error` for observability — the row stays
/// `awaiting`, the next tick tries again.
///
/// # Errors
/// Any sqlx error.
pub async fn record_poll_error(pool: &SqlitePool, award_id: &str, message: &str) -> Result<()> {
    sqlx::query("UPDATE awards SET last_poll_error = ? WHERE award_id = ?")
        .bind(message)
        .bind(award_id)
        .execute(pool)
        .await?;
    Ok(())
}
