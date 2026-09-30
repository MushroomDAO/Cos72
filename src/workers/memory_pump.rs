//! docs/agent/spec.md「记忆泵（memory_pump，单任务）」: the ONE task that ever
//! calls [`crate::kernel::KernelPort::remember_once`] (docs/agent/
//! architecture.md 不可动摇的边界 #4 — HTTP handlers and `award_poller` must
//! never call it directly, structurally checked by `tests/structure.rs`).
//! Drains `outbox` rows one at a time (spec.md: "单一泵串行调
//! remember_once" — `remember_once` is not atomic on the wire, so only ever
//! having ONE caller in flight per `dedup_key` is what keeps a retry safe,
//! ME4-S3 §3.5).

use std::sync::Arc;
use std::time::Duration;

use agent24_os_sdk::{ClientError, RememberOnce};
use serde_json::Value;
use sqlx::SqlitePool;

use crate::core::now_rfc3339;
use crate::kernel::KernelPort;
use crate::store::outbox::{bump_retry, fetch_due_pending, mark_dead, mark_done};
use crate::store::{Cos72Store, StoreError};

/// A ⚖️ conservative scan interval for due `outbox` rows — spec.md does not
/// pin one explicitly for this pump (unlike `award_poller`'s spec'd 3s), so
/// this mirrors that same order of magnitude; a `credit_award` commit only
/// ever ADDS a `pending` row (never removes the need to scan), so a slightly
/// slower/faster tick only changes latency, never correctness.
pub const PUMP_INTERVAL: Duration = Duration::from_secs(2);

/// spec.md「记忆泵」: "退避 1s×2^n，上限 5min ⚖️" — `attempts_after_increment`
/// is the row's `attempts` value AFTER this failure is recorded, so the
/// FIRST retryable failure (`== 1`) waits 1s, the second 2s, ... capped at
/// 300s. Ported verbatim from `workers::award_poller`'s sibling reasoning
/// (itself ported from Sin90's `reconciler::backoff_after`).
fn backoff_after(attempts_after_increment: i64) -> Duration {
    let exponent = attempts_after_increment.saturating_sub(1).max(0) as u32;
    let secs = 1_u64.checked_shl(exponent).unwrap_or(u64::MAX);
    Duration::from_secs(secs.min(300))
}

fn iso8601_after(duration: Duration) -> String {
    let delta = chrono::Duration::from_std(duration).unwrap_or(chrono::Duration::seconds(300));
    (chrono::Utc::now() + delta)
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// What [`run_once`] tells [`spawn_loop`] to do next — same shape as
/// `workers::award_poller::TickControl`, kept as its own type (not shared)
/// since the two pumps' futures have nothing else in common and a shared
/// type would only add an import for no real reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickControl {
    Continue,
    Stop,
}

/// docs/agent/spec.md「记忆泵」错误处理: `ClientError`'s 18 variants sorted
/// into "permanent" (`dead`), "retryable" (stays `pending`, backs off), or
/// "stop the whole pump" (`ConnectionLost`) — written as a wildcard-free
/// `match` (ME4-S3 §2.6 / FU-87) so a FUTURE variant added to `ClientError`
/// fails this crate's build instead of silently falling through a `_` arm
/// into the wrong bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Classification {
    Permanent,
    Retryable,
    Stop,
}

fn classify(err: &ClientError) -> Classification {
    match err {
        ClientError::ConnectionLost => Classification::Stop,
        ClientError::Forbidden(_)
        | ClientError::QuotaExceeded(_)
        | ClientError::InvalidParams(_)
        | ClientError::TokenInvalid(_)
        | ClientError::PayloadTooLarge(_) => Classification::Permanent,
        ClientError::RateLimited(_)
        | ClientError::Busy(_)
        | ClientError::Timeout(_)
        | ClientError::NotReady(_)
        | ClientError::Draining(_)
        | ClientError::NotSent(_)
        | ClientError::RequestNotInFlight(_)
        | ClientError::Revoked(_)
        | ClientError::NotFound(_)
        | ClientError::Cancelled
        | ClientError::Other(_) => Classification::Retryable,
        ClientError::Unavailable { retryable, .. } => {
            if *retryable {
                Classification::Retryable
            } else {
                Classification::Permanent
            }
        }
    }
}

/// One scan over every currently-due `pending` outbox row (docs/agent/
/// spec.md「记忆泵」). `kind` is always `"task.summary"` — the outbox schema
/// itself only allows `kind IN ('memory.remember')` for the ROW's own kind
/// (the outbox mechanism), which is a different axis from this string (the
/// MEMORY client's own free-form category, same distinction
/// `agent-speaker`/Sin90's own reconciler draws).
///
/// # Errors
/// Any sqlx error from the store calls this makes (a row's own kernel
/// answer never propagates as an `Err` here — it is always translated into
/// a `done`/`dead`/retry write instead).
pub async fn run_once<K: KernelPort>(
    pool: &SqlitePool,
    kernel: &K,
) -> Result<TickControl, StoreError> {
    let now = now_rfc3339();
    let rows = fetch_due_pending(pool, &now).await?;
    for row in rows {
        let body = match serde_json::from_str::<Value>(&row.payload) {
            Ok(Value::Object(map)) => map,
            Ok(_) | Err(_) => {
                // A malformed payload can never become valid by retrying —
                // permanent, same posture as `ClientError::InvalidParams`.
                mark_dead(
                    pool,
                    row.id,
                    "outbox payload is not a JSON object; cannot remember it",
                )
                .await?;
                continue;
            }
        };

        match kernel
            .remember_once("task.summary", &row.dedup_key, body)
            .await
        {
            Ok(RememberOnce::Created { id, .. } | RememberOnce::Found { id }) => {
                mark_done(pool, row.id, &id).await?;
            }
            Ok(RememberOnce::Inconclusive) => {
                // spec.md: "Inconclusive → 退避重试（不当作不存在）".
                let attempts = row.attempts + 1;
                let next_attempt_at = iso8601_after(backoff_after(attempts));
                bump_retry(
                    pool,
                    row.id,
                    &next_attempt_at,
                    Some("remember_once: recall pre-check inconclusive"),
                )
                .await?;
            }
            Err(err) => match classify(&err) {
                Classification::Stop => return Ok(TickControl::Stop),
                Classification::Permanent => {
                    mark_dead(pool, row.id, &err.to_string()).await?;
                }
                Classification::Retryable => {
                    let attempts = row.attempts + 1;
                    let next_attempt_at = iso8601_after(backoff_after(attempts));
                    bump_retry(pool, row.id, &next_attempt_at, Some(&err.to_string())).await?;
                }
            },
        }
    }
    Ok(TickControl::Continue)
}

/// Production entry point — `main.rs` calls this only when `kernel.
/// memory_available()` (docs/agent/spec.md「记忆泵」: "没有 memory 能力 … → 泵
/// 不启动，outbox 行保持 pending，不报错"). Takes an owned [`Cos72Store`] for
/// the same cross-crate `pub(crate)` reason as `workers::award_poller::
/// spawn_loop`.
#[must_use]
pub fn spawn_loop<K: KernelPort>(store: Cos72Store, kernel: Arc<K>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PUMP_INTERVAL);
        loop {
            interval.tick().await;
            match run_once(store.pool(), kernel.as_ref()).await {
                Ok(TickControl::Continue) => {}
                Ok(TickControl::Stop) => {
                    tracing::warn!("cos72: memory_pump stopping — kernel callback connection lost");
                    break;
                }
                Err(err) => {
                    tracing::error!(
                        error = %err,
                        "cos72: memory_pump store error this tick, will retry next tick"
                    );
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::RecordingKernelPort;
    use crate::store::ledger::credit_award;
    use crate::store::tasks::{self as store_tasks, ClaimOutcome, NewTask, SubmitOutcome};
    use crate::store::Cos72Store;
    use serde_json::json;

    async fn completed_task(store: &Cos72Store, task_id: &str, title: &str, points: i64) {
        store_tasks::insert_task(
            store.pool(),
            NewTask {
                task_id,
                title,
                description: "",
                reward_points: points,
                publisher: "pub1",
                created_at: "2026-01-01T00:00:00.000Z",
            },
        )
        .await
        .unwrap();
        let ClaimOutcome::Claimed(_) =
            store_tasks::claim_task(store.pool(), task_id, "mem1", "2026-01-01T00:00:01.000Z")
                .await
                .unwrap()
        else {
            panic!("claim must succeed");
        };
        let outcome = store_tasks::submit_task(
            store.pool(),
            task_id,
            "mem1",
            None,
            "2026-01-01T00:00:02.000Z",
            &format!("awd_{task_id}"),
        )
        .await
        .unwrap();
        let SubmitOutcome::NeedsAdvise { award_id, .. } = outcome else {
            panic!("expected NeedsAdvise");
        };
        let approval_id = format!("appr_{task_id}");
        store_tasks::cas_write_approval_id(store.pool(), &award_id, &approval_id)
            .await
            .unwrap();
        let outcome = credit_award(store.pool(), &approval_id, "2026-01-01T00:00:03.000Z")
            .await
            .unwrap();
        assert_eq!(outcome, crate::store::ledger::CreditOutcome::Credited);
    }

    /// docs/agent/tasks.md T1.3.1 验收命令 #4
    /// `completion_enqueues_exactly_one_outbox_row`: completing a task (via
    /// `credit_award`, T1.3.1a's own transaction) leaves exactly one
    /// `pending` outbox row, keyed `cos72:task:<id>:completed`, carrying
    /// `title`.
    #[tokio::test]
    async fn completion_enqueues_exactly_one_outbox_row() {
        let store = Cos72Store::open_memory().await.unwrap();
        completed_task(&store, "tsk_1", "do the thing", 10).await;

        let rows: Vec<(String, String, String)> =
            sqlx::query_as("SELECT dedup_key, payload, state FROM outbox")
                .fetch_all(store.pool())
                .await
                .unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0, "cos72:task:tsk_1:completed");
        assert_eq!(rows[0].2, "pending");
        let payload: Value = serde_json::from_str(&rows[0].1).unwrap();
        assert_eq!(payload["task_id"], "tsk_1");
        assert_eq!(payload["title"], "do the thing");
        assert_eq!(payload["member"], "mem1");
        assert_eq!(payload["points"], 10);
        assert_eq!(payload["award_id"], "awd_tsk_1");
        assert_eq!(payload["approval_id"], "appr_tsk_1");
    }

    /// docs/agent/tasks.md T1.3.1 验收命令 #4
    /// `pump_calls_remember_once_with_namespaced_dedup_key`.
    #[tokio::test]
    async fn pump_calls_remember_once_with_namespaced_dedup_key() {
        let store = Cos72Store::open_memory().await.unwrap();
        completed_task(&store, "tsk_1", "do the thing", 10).await;
        let kernel = RecordingKernelPort::new();
        kernel.push_remember_response(Ok(RememberOnce::Created {
            id: "osmem:1".to_owned(),
            at: "2026-01-01T00:00:04.000Z".to_owned(),
        }));

        let control = run_once(store.pool(), &kernel).await.unwrap();
        assert_eq!(control, TickControl::Continue);

        let calls = kernel.remember_calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0].kind, "task.summary");
        assert_eq!(calls[0].dedup_key, "cos72:task:tsk_1:completed");
        assert_eq!(calls[0].body["task_id"], json!("tsk_1"));

        let row: (String, Option<String>) =
            sqlx::query_as("SELECT state, result_ref FROM outbox WHERE dedup_key = ?")
                .bind("cos72:task:tsk_1:completed")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(row.0, "done");
        assert_eq!(row.1.as_deref(), Some("osmem:1"));
    }

    /// docs/agent/tasks.md T1.3.1 验收命令 #4
    /// `inconclusive_is_retried_not_marked_done`: 正对照 is the previous test
    /// (`Created` really does mark `done`).
    #[tokio::test]
    async fn inconclusive_is_retried_not_marked_done() {
        let store = Cos72Store::open_memory().await.unwrap();
        completed_task(&store, "tsk_1", "t", 10).await;
        let kernel = RecordingKernelPort::new();
        kernel.push_remember_response(Ok(RememberOnce::Inconclusive));

        run_once(store.pool(), &kernel).await.unwrap();

        let row: (String, i64, Option<String>) =
            sqlx::query_as("SELECT state, attempts, result_ref FROM outbox WHERE dedup_key = ?")
                .bind("cos72:task:tsk_1:completed")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(
            row.0, "pending",
            "Inconclusive must not be treated as absent"
        );
        assert_eq!(row.1, 1);
        assert_eq!(row.2, None);
    }

    /// 变异验证 target: a permanent error (`Forbidden`) marks the row `dead`
    /// with `last_error` set, and does NOT retry it on a second tick.
    #[tokio::test]
    async fn permanent_error_marks_dead_and_does_not_retry() {
        let store = Cos72Store::open_memory().await.unwrap();
        completed_task(&store, "tsk_1", "t", 10).await;
        let kernel = RecordingKernelPort::new();
        kernel.push_remember_response(Err(ClientError::Forbidden("no memory for you".into())));

        run_once(store.pool(), &kernel).await.unwrap();
        let row: (String, Option<String>) =
            sqlx::query_as("SELECT state, last_error FROM outbox WHERE dedup_key = ?")
                .bind("cos72:task:tsk_1:completed")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(row.0, "dead");
        assert!(row.1.unwrap().contains("no memory for you"));

        // Second tick: a `dead` row is never re-fetched by `fetch_due_pending`
        // (it only selects `state = 'pending'`) — no second remember_once
        // call at all.
        run_once(store.pool(), &kernel).await.unwrap();
        assert_eq!(kernel.remember_calls().len(), 1);
    }

    /// A retryable error (`Timeout`) leaves the row `pending` with a pushed
    /// `next_attempt_at` — 正对照 to the permanent case above.
    #[tokio::test]
    async fn retryable_error_stays_pending_with_backoff() {
        let store = Cos72Store::open_memory().await.unwrap();
        completed_task(&store, "tsk_1", "t", 10).await;
        let kernel = RecordingKernelPort::new();
        kernel.push_remember_response(Err(ClientError::Timeout("slow".into())));

        run_once(store.pool(), &kernel).await.unwrap();
        let row: (String, i64) =
            sqlx::query_as("SELECT state, attempts FROM outbox WHERE dedup_key = ?")
                .bind("cos72:task:tsk_1:completed")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(row.0, "pending");
        assert_eq!(row.1, 1);
    }

    /// `ConnectionLost` stops the pump for this tick without marking
    /// anything dead or bumping retry — the row is left exactly as it was,
    /// a fresh generation's pump resumes it untouched.
    #[tokio::test]
    async fn connection_lost_stops_the_pump_without_touching_the_row() {
        let store = Cos72Store::open_memory().await.unwrap();
        completed_task(&store, "tsk_1", "t", 10).await;
        let kernel = RecordingKernelPort::new();
        kernel.push_remember_response(Err(ClientError::ConnectionLost));

        let control = run_once(store.pool(), &kernel).await.unwrap();
        assert_eq!(control, TickControl::Stop);
        let row: (String, i64) =
            sqlx::query_as("SELECT state, attempts FROM outbox WHERE dedup_key = ?")
                .bind("cos72:task:tsk_1:completed")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(row.0, "pending");
        assert_eq!(
            row.1, 0,
            "the row must be untouched, not counted as an attempt"
        );
    }

    /// Sanity check that `classify` is exhaustive and stable for a
    /// representative sample of the closed set (the real guarantee — "a new
    /// `ClientError` variant fails the BUILD" — is enforced by the compiler
    /// itself via the wildcard-free `match`, not by this test).
    #[test]
    fn classify_sample() {
        assert_eq!(classify(&ClientError::ConnectionLost), Classification::Stop);
        assert_eq!(
            classify(&ClientError::Forbidden("x".into())),
            Classification::Permanent
        );
        assert_eq!(
            classify(&ClientError::Timeout("x".into())),
            Classification::Retryable
        );
        assert_eq!(
            classify(&ClientError::Unavailable {
                retryable: true,
                cause: agent24_os_sdk::UnavailableCause::NoProvider
            }),
            Classification::Retryable
        );
        assert_eq!(
            classify(&ClientError::Unavailable {
                retryable: false,
                cause: agent24_os_sdk::UnavailableCause::NoProvider
            }),
            Classification::Permanent
        );
    }
}
