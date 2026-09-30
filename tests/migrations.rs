//! docs/agent/tasks.md T1.1.1 验收命令 #2: constraint positive/negative
//! samples for `migrations/0001_init.sql` (docs/agent/spec.md「数据模型」).
//! Every test does a LEGAL insert first (proving the schema accepts the
//! normal case), then the violating write (proving the schema, not
//! application code, rejects it).

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

/// A fresh, fully-migrated database per test — a unique named shared-cache
/// in-memory database (same technique as `cos72::store::Cos72Store::
/// open_memory`, reimplemented here rather than imported: this is an
/// integration test crate, and asserting the constraint directly at the SQL
/// layer should not depend on the library's own repository code existing or
/// being correct — T1.2.1/T1.3.1 have not been written yet).
async fn fresh_db() -> SqlitePool {
    let uri = format!(
        "sqlite:file:cos72-migrations-test-{}?mode=memory&cache=shared",
        ulid::Ulid::new()
    );
    let options = SqliteConnectOptions::from_str(&uri)
        .unwrap()
        .busy_timeout(std::time::Duration::from_millis(5000))
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}

async fn insert_task(pool: &SqlitePool, task_id: &str, status: &str, claimer: Option<&str>) {
    sqlx::query(
        "INSERT INTO tasks (task_id, title, reward_points, publisher, claimer, status, \
         created_at, updated_at) VALUES (?, 'title', 10, 'pub1', ?, ?, '2026-09-30T00:00:00Z', \
         '2026-09-30T00:00:00Z')",
    )
    .bind(task_id)
    .bind(claimer)
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_award(
    pool: &SqlitePool,
    award_id: &str,
    task_id: &str,
    approval_id: Option<&str>,
    state: &str,
) {
    sqlx::query(
        "INSERT INTO awards (award_id, task_id, member, points, approval_id, state, \
         created_at) VALUES (?, ?, 'member1', 10, ?, ?, '2026-09-30T00:00:00Z')",
    )
    .bind(award_id)
    .bind(task_id)
    .bind(approval_id)
    .bind(state)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_ledger_row(pool: &SqlitePool, award_id: &str, task_id: &str, approval_id: &str) {
    sqlx::query(
        "INSERT INTO points_ledger (award_id, member, delta, task_id, approval_id, \
         created_at) VALUES (?, 'member1', 10, ?, ?, '2026-09-30T00:00:00Z')",
    )
    .bind(award_id)
    .bind(task_id)
    .bind(approval_id)
    .execute(pool)
    .await
    .unwrap();
}

/// docs/agent/architecture.md 不可动摇的边界 #5: "账本只追加"。
///
/// 变异 (docs/agent/tasks.md T1.1.1 验收命令 #2): delete
/// `trg_points_ledger_no_update` from `migrations/0001_init.sql` — this test
/// goes red (verified by hand for this PR; see PR body).
#[tokio::test]
async fn ledger_rejects_update() {
    let pool = fresh_db().await;
    insert_task(&pool, "tsk_1", "submitted", Some("member1")).await;
    insert_award(&pool, "awd_1", "tsk_1", Some("apr_1"), "awaiting").await;
    // Positive: a legal insert succeeds.
    insert_ledger_row(&pool, "awd_1", "tsk_1", "apr_1").await;

    let err = sqlx::query("UPDATE points_ledger SET delta = 20 WHERE award_id = 'awd_1'")
        .execute(&pool)
        .await
        .expect_err("UPDATE on points_ledger must fail");
    assert!(
        err.to_string().contains("append-only"),
        "unexpected error: {err}"
    );
}

/// 变异: delete `trg_points_ledger_no_delete` — this test goes red (verified
/// by hand for this PR; see PR body).
#[tokio::test]
async fn ledger_rejects_delete() {
    let pool = fresh_db().await;
    insert_task(&pool, "tsk_1", "submitted", Some("member1")).await;
    insert_award(&pool, "awd_1", "tsk_1", Some("apr_1"), "awaiting").await;
    insert_ledger_row(&pool, "awd_1", "tsk_1", "apr_1").await;

    let err = sqlx::query("DELETE FROM points_ledger WHERE award_id = 'awd_1'")
        .execute(&pool)
        .await
        .expect_err("DELETE on points_ledger must fail");
    assert!(
        err.to_string().contains("append-only"),
        "unexpected error: {err}"
    );
}

/// `award_id` is `UNIQUE` on `points_ledger` (docs/agent/spec.md「数据模型」
/// points_ledger 行): the same award can never be credited twice.
///
/// 变异: drop the `UNIQUE` on `points_ledger.award_id` — this test goes red
/// (verified by hand for this PR; see PR body).
#[tokio::test]
async fn duplicate_award_id_in_ledger_rejected() {
    let pool = fresh_db().await;
    insert_task(&pool, "tsk_1", "submitted", Some("member1")).await;
    insert_award(&pool, "awd_1", "tsk_1", Some("apr_1"), "awaiting").await;
    // Positive: the first insert for this award succeeds.
    insert_ledger_row(&pool, "awd_1", "tsk_1", "apr_1").await;

    let err = sqlx::query(
        "INSERT INTO points_ledger (award_id, member, delta, task_id, approval_id, \
         created_at) VALUES ('awd_1', 'member1', 10, 'tsk_1', 'apr_1', '2026-09-30T00:00:01Z')",
    )
    .execute(&pool)
    .await
    .expect_err("a second ledger row for the same award_id must fail");
    assert!(
        err.as_database_error().unwrap().is_unique_violation(),
        "unexpected error: {err}"
    );
}

/// A task can have at most one `awaiting` award at a time
/// (docs/agent/spec.md「数据模型」awards 行: "一个任务同时只有一笔在审").
///
/// 变异: drop `idx_awards_one_awaiting_per_task` — this test goes red
/// (verified by hand for this PR; see PR body).
#[tokio::test]
async fn second_awaiting_award_for_same_task_rejected() {
    let pool = fresh_db().await;
    insert_task(&pool, "tsk_1", "submitted", Some("member1")).await;
    // Positive: the first `awaiting` award for this task succeeds.
    insert_award(&pool, "awd_1", "tsk_1", None, "awaiting").await;

    let err = sqlx::query(
        "INSERT INTO awards (award_id, task_id, member, points, approval_id, state, \
         created_at) VALUES ('awd_2', 'tsk_1', 'member1', 10, NULL, 'awaiting', \
         '2026-09-30T00:00:01Z')",
    )
    .execute(&pool)
    .await
    .expect_err("a second awaiting award for the same task must fail");
    assert!(
        err.as_database_error().unwrap().is_unique_violation(),
        "unexpected error: {err}"
    );
}

/// `CHECK((status='open') = (claimer IS NULL))` (docs/agent/spec.md「数据模
/// 型」tasks 行): an `open` task can never carry a `claimer`.
///
/// 变异: drop that `CHECK` from `migrations/0001_init.sql` — this test goes
/// red (verified by hand for this PR; see PR body).
#[tokio::test]
async fn open_task_with_claimer_rejected() {
    let pool = fresh_db().await;
    // Positive: `open` with no claimer succeeds.
    insert_task(&pool, "tsk_1", "open", None).await;

    let err = sqlx::query(
        "INSERT INTO tasks (task_id, title, reward_points, publisher, claimer, status, \
         created_at, updated_at) VALUES ('tsk_2', 'title', 10, 'pub1', 'member1', 'open', \
         '2026-09-30T00:00:00Z', '2026-09-30T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect_err("an open task with a non-null claimer must fail its CHECK constraint");
    assert!(
        err.as_database_error()
            .unwrap()
            .message()
            .contains("CHECK constraint failed"),
        "unexpected error: {err}"
    );
}
