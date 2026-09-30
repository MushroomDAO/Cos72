//! docs/agent/spec.md「入账（award_poller，单任务，间隔 3 秒 ⚖️）」: polls
//! `approval_status` for every `awaiting` award that already has a recorded
//! `approval_id`, and turns `approved`/`denied`/`timed_out` into the
//! corresponding store transition (`store::ledger`). Runs as its own tokio
//! task ([`spawn_loop`]); [`run_once`] is the same logic pulled out so tests
//! (and a future generation's startup, which needs no separate "catch up"
//! pass — it just calls this on its own schedule) can drive one scan
//! deterministically.

use std::sync::Arc;
use std::time::Duration;

use agent24_os_sdk::{ApprovalDecision, ClientError};
use serde_json::{json, Map};
use sqlx::SqlitePool;

use crate::core::now_rfc3339;
use crate::kernel::KernelPort;
use crate::store::ledger::{credit_award, deny_award, expire_award, CreditOutcome};
use crate::store::tasks::{list_awards_pending_poll, record_poll_error};
use crate::store::{Cos72Store, StoreError};

/// spec.md「入账」: "间隔 3 秒 ⚖️".
pub const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// What [`run_once`] tells [`spawn_loop`] to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickControl {
    /// Keep polling on the next tick.
    Continue,
    /// The callback connection is gone (`ClientError::ConnectionLost`) — the
    /// SDK's own fatal hook is about to `exit(70)` this generation; this
    /// task should stop polling rather than spin on a dead connection.
    Stop,
}

fn task_payload(task_id: &str, award_id: &str) -> Map<String, serde_json::Value> {
    let mut payload = Map::new();
    payload.insert("task_id".to_owned(), json!(task_id));
    payload.insert("award_id".to_owned(), json!(award_id));
    payload
}

/// One scan over every `awaiting` award with a recorded `approval_id`
/// (docs/agent/spec.md「入账」). Never errors on an individual row's kernel
/// answer — only a store (sqlx) error propagates, since that means Cos72's
/// OWN database is unhealthy, a different failure mode than "the kernel
/// said something about approval X".
///
/// # Errors
/// Any sqlx error from the store calls this makes.
pub async fn run_once<K: KernelPort>(
    pool: &SqlitePool,
    kernel: &K,
) -> Result<TickControl, StoreError> {
    let rows = list_awards_pending_poll(pool).await?;
    for row in rows {
        let Some(approval_id) = row.approval_id.clone() else {
            // `list_awards_pending_poll` only selects rows with `approval_id
            // IS NOT NULL` — unreachable in practice, but this loop does not
            // trust that invariant blindly.
            continue;
        };
        match kernel.approval_status(&approval_id).await {
            Ok(answer) => {
                let now = now_rfc3339();
                match answer.decision {
                    ApprovalDecision::Pending => {}
                    ApprovalDecision::Approved => {
                        if credit_award(pool, &approval_id, &now).await? == CreditOutcome::Credited
                        {
                            let mut payload = task_payload(&row.task_id, &row.award_id);
                            payload.insert("approval_id".to_owned(), json!(approval_id));
                            payload.insert("member".to_owned(), json!(row.member));
                            payload.insert("points".to_owned(), json!(row.points));
                            kernel.emit("award.credited", payload);
                            kernel
                                .emit("task.completed", task_payload(&row.task_id, &row.award_id));
                        }
                    }
                    ApprovalDecision::Denied => {
                        if deny_award(pool, &approval_id, &now).await? == CreditOutcome::Credited {
                            kernel.emit("award.denied", task_payload(&row.task_id, &row.award_id));
                        }
                    }
                    ApprovalDecision::TimedOut => {
                        if expire_award(pool, &approval_id, &now).await? == CreditOutcome::Credited
                        {
                            kernel.emit("award.expired", task_payload(&row.task_id, &row.award_id));
                        }
                    }
                }
            }
            Err(ClientError::NotFound(message)) => {
                // spec.md「入账」错误: "NotFound → 视同 expired（内核已无此审批）
                // 并记 last_poll_error".
                record_poll_error(pool, &row.award_id, &message).await?;
                let now = now_rfc3339();
                if expire_award(pool, &approval_id, &now).await? == CreditOutcome::Credited {
                    kernel.emit("award.expired", task_payload(&row.task_id, &row.award_id));
                }
            }
            Err(ClientError::ConnectionLost) => {
                return Ok(TickControl::Stop);
            }
            Err(other) => {
                // Retryable or unclassified: record for observability, try
                // again next tick — the row stays `awaiting`.
                record_poll_error(pool, &row.award_id, &other.to_string()).await?;
            }
        }
    }
    Ok(TickControl::Continue)
}

/// Production entry point — `main.rs` calls this only when `kernel.
/// approval_available()` (docs/agent/architecture.md 不可动摇的边界 #6: "只
/// 声明真正用到的能力"), the same "no handle, no task" posture Sin90's own
/// pump loops use. A NEW generation's poller starts here and immediately
/// resumes from whatever `awaiting` rows are in the database — no separate
/// "catch up" pass, since [`run_once`] IS that pass, run on a timer.
///
/// Takes an owned [`Cos72Store`] (cheap to clone — an `Arc`-backed pool)
/// rather than a raw `SqlitePool`, so `main.rs` — a SEPARATE crate from this
/// library (the `[[bin]]`/`[lib]` split in `Cargo.toml`) — never needs
/// `Cos72Store::pool()` to be more than `pub(crate)`.
#[must_use]
pub fn spawn_loop<K: KernelPort>(store: Cos72Store, kernel: Arc<K>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        loop {
            interval.tick().await;
            match run_once(store.pool(), kernel.as_ref()).await {
                Ok(TickControl::Continue) => {}
                Ok(TickControl::Stop) => {
                    tracing::warn!(
                        "cos72: award_poller stopping — kernel callback connection lost"
                    );
                    break;
                }
                Err(err) => {
                    tracing::error!(
                        error = %err,
                        "cos72: award_poller store error this tick, will retry next tick"
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
    use crate::store::tasks::{self as store_tasks, ClaimOutcome, NewTask, SubmitOutcome};
    use crate::store::Cos72Store;
    use agent24_os_sdk::{ApprovalAnswer, ApprovalKind};

    async fn publish_claim_submit(store: &Cos72Store) -> (String, String) {
        let task_id = "tsk_1".to_owned();
        store_tasks::insert_task(
            store.pool(),
            NewTask {
                task_id: &task_id,
                title: "t",
                description: "",
                reward_points: 10,
                publisher: "pub1",
                created_at: "2026-01-01T00:00:00.000Z",
            },
        )
        .await
        .unwrap();
        let ClaimOutcome::Claimed(_) =
            store_tasks::claim_task(store.pool(), &task_id, "mem1", "2026-01-01T00:00:01.000Z")
                .await
                .unwrap()
        else {
            panic!("claim must succeed");
        };
        let outcome = store_tasks::submit_task(
            store.pool(),
            &task_id,
            "mem1",
            None,
            "2026-01-01T00:00:02.000Z",
            "awd_1",
        )
        .await
        .unwrap();
        let SubmitOutcome::NeedsAdvise { award_id, .. } = outcome else {
            panic!("expected NeedsAdvise");
        };
        store_tasks::cas_write_approval_id(store.pool(), &award_id, "appr-1")
            .await
            .unwrap();
        (task_id, award_id)
    }

    fn answer(decision: ApprovalDecision) -> ApprovalAnswer {
        ApprovalAnswer {
            approval_id: "appr-1".to_owned(),
            kind: ApprovalKind::Advise,
            binding: false,
            decision,
            executed_at: None,
        }
    }

    /// docs/agent/tasks.md T1.3.1 验收命令 #2
    /// `approved_credits_exactly_once`: three ticks, all `approved` — the
    /// ledger ends up with exactly one row for this `award_id`.
    #[tokio::test]
    async fn approved_credits_exactly_once() {
        let store = Cos72Store::open_memory().await.unwrap();
        let (_task_id, award_id) = publish_claim_submit(&store).await;
        let kernel = RecordingKernelPort::new();
        kernel.set_status("appr-1", Ok(answer(ApprovalDecision::Approved)));

        for _ in 0..3 {
            run_once(store.pool(), &kernel).await.unwrap();
        }

        let (_balances, entries) = crate::store::ledger::all_balances(store.pool())
            .await
            .unwrap();
        assert_eq!(entries, 1);
        let rows: Vec<(i64,)> =
            sqlx::query_as("SELECT COUNT(*) FROM points_ledger WHERE award_id = ?")
                .bind(&award_id)
                .fetch_all(store.pool())
                .await
                .unwrap();
        assert_eq!(rows[0].0, 1, "must be credited exactly once");
        let emitted = kernel.emitted();
        assert_eq!(
            emitted
                .iter()
                .filter(|(kind, _)| kind == "award.credited")
                .count(),
            1,
            "{emitted:?}"
        );
    }

    /// `approved_orphan_is_never_credited`: the kernel reports `approved`
    /// for an `approval_id` NOT in `awards` at all — nothing is credited.
    /// 正对照: the SAME poll also processes the real award (`appr-1`), which
    /// DOES get credited — proving the orphan id was the reason the other
    /// one was skipped, not a poller that credits nothing at all.
    #[tokio::test]
    async fn approved_orphan_is_never_credited() {
        let store = Cos72Store::open_memory().await.unwrap();
        let (_task_id, award_id) = publish_claim_submit(&store).await;
        let kernel = RecordingKernelPort::new();
        kernel.set_status("appr-1", Ok(answer(ApprovalDecision::Approved)));

        // Directly exercise the store-level guarantee for an id the poller
        // never even sees in `awards`.
        let now = now_rfc3339();
        let outcome = crate::store::ledger::credit_award(store.pool(), "appr-orphan", &now)
            .await
            .unwrap();
        assert_eq!(outcome, CreditOutcome::UnknownAward);

        run_once(store.pool(), &kernel).await.unwrap();
        let rows: Vec<(i64,)> =
            sqlx::query_as("SELECT COUNT(*) FROM points_ledger WHERE award_id = ?")
                .bind(&award_id)
                .fetch_all(store.pool())
                .await
                .unwrap();
        assert_eq!(rows[0].0, 1, "the real award must still be credited");
        let total: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM points_ledger")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(total.0, 1, "the orphan must not have added a second row");
    }

    /// `denied_returns_task_to_claimed_without_ledger`.
    #[tokio::test]
    async fn denied_returns_task_to_claimed_without_ledger() {
        let store = Cos72Store::open_memory().await.unwrap();
        let (task_id, _award_id) = publish_claim_submit(&store).await;
        let kernel = RecordingKernelPort::new();
        kernel.set_status("appr-1", Ok(answer(ApprovalDecision::Denied)));

        run_once(store.pool(), &kernel).await.unwrap();

        let task = store_tasks::get_task(store.pool(), &task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.status, "claimed");
        let total: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM points_ledger")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(total.0, 0);
        assert!(kernel
            .emitted()
            .iter()
            .any(|(kind, _)| kind == "award.denied"));
    }

    /// `timed_out_marks_expired_and_resubmit_creates_new_award`.
    #[tokio::test]
    async fn timed_out_marks_expired_and_resubmit_creates_new_award() {
        let store = Cos72Store::open_memory().await.unwrap();
        let (task_id, first_award_id) = publish_claim_submit(&store).await;
        let kernel = RecordingKernelPort::new();
        kernel.set_status("appr-1", Ok(answer(ApprovalDecision::TimedOut)));

        run_once(store.pool(), &kernel).await.unwrap();

        let task = store_tasks::get_task(store.pool(), &task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.status, "submitted", "spec.md: task stays submitted");
        let state: (String,) = sqlx::query_as("SELECT state FROM awards WHERE award_id = ?")
            .bind(&first_award_id)
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(state.0, "expired");

        // Resubmit: no `awaiting` row left for this task → a NEW award.
        let outcome = store_tasks::submit_task(
            store.pool(),
            &task_id,
            "mem1",
            None,
            "2026-01-01T00:10:00.000Z",
            "awd_2",
        )
        .await
        .unwrap();
        let SubmitOutcome::NeedsAdvise { award_id, .. } = outcome else {
            panic!("expected NeedsAdvise");
        };
        assert_eq!(award_id, "awd_2", "must be a genuinely new award id");
        assert_ne!(award_id, first_award_id);
    }

    /// `not_found_marks_expired`.
    #[tokio::test]
    async fn not_found_marks_expired() {
        let store = Cos72Store::open_memory().await.unwrap();
        let (_task_id, award_id) = publish_claim_submit(&store).await;
        let kernel = RecordingKernelPort::new();
        kernel.set_status("appr-1", Err(ClientError::NotFound("gone".into())));

        run_once(store.pool(), &kernel).await.unwrap();

        let state: (String, Option<String>) =
            sqlx::query_as("SELECT state, last_poll_error FROM awards WHERE award_id = ?")
                .bind(&award_id)
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(state.0, "expired");
        assert_eq!(state.1.as_deref(), Some("gone"));
    }

    /// 变异验证 (docs/agent/tasks.md T1.3.1 验收命令 #2): if the re-confirm
    /// "still awaiting" check inside `credit_award` were removed, the
    /// `award_id UNIQUE` constraint on `points_ledger` alone must still stop
    /// a double credit — this test pins that second line of defense
    /// directly (PR body records the reading with each guard removed).
    #[tokio::test]
    async fn ledger_unique_constraint_alone_still_prevents_double_credit() {
        let store = Cos72Store::open_memory().await.unwrap();
        let (_task_id, award_id) = publish_claim_submit(&store).await;
        let now = now_rfc3339();
        let first = crate::store::ledger::credit_award(store.pool(), "appr-1", &now)
            .await
            .unwrap();
        assert_eq!(first, CreditOutcome::Credited);
        // Force the award row back to `awaiting` to bypass the
        // "still-awaiting" guard, simulating that check having been removed.
        sqlx::query("UPDATE awards SET state = 'awaiting' WHERE award_id = ?")
            .bind(&award_id)
            .execute(store.pool())
            .await
            .unwrap();
        let second = crate::store::ledger::credit_award(store.pool(), "appr-1", &now)
            .await
            .unwrap();
        assert_eq!(
            second,
            CreditOutcome::AlreadyHandled,
            "the ledger's own award_id UNIQUE constraint must still catch this"
        );
        let total: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM points_ledger")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(total.0, 1);
    }
}
