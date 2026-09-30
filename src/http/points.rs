//! docs/agent/spec.md「REST 路由」`GET /points` / `GET /points/{member}` —
//! read-only views over `store::ledger`'s replay. Neither route ever writes
//! anything; crediting only ever happens inside `workers::award_poller`.

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use crate::http::error::ApiError;
use crate::http::Cos72State;
use crate::kernel::KernelPort;
use crate::store::ledger;
use crate::store::StoreError;

fn store_error(err: StoreError) -> ApiError {
    tracing::error!(error = %err, "cos72: store error");
    ApiError::Internal
}

/// docs/agent/spec.md「REST 路由」`GET /points`: `{balances: [{member,
/// balance}], entries: n}` — every balance is the ledger replay, `entries`
/// is the ledger's own total row count.
pub async fn all_balances<K: KernelPort>(
    State(state): State<Cos72State<K>>,
) -> Result<Json<Value>, ApiError> {
    let (balances, entries) = ledger::all_balances(state.store.pool())
        .await
        .map_err(store_error)?;
    let balances: Vec<Value> = balances
        .into_iter()
        .map(|b| json!({"member": b.member, "balance": b.balance}))
        .collect();
    Ok(Json(json!({ "balances": balances, "entries": entries })))
}

/// docs/agent/spec.md「REST 路由」`GET /points/{member}`: balance (0 if no
/// rows) plus every ledger entry for that member.
pub async fn member_balance<K: KernelPort>(
    State(state): State<Cos72State<K>>,
    Path(member): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let balance = ledger::balance_for_member(state.store.pool(), &member)
        .await
        .map_err(store_error)?;
    let entries = ledger::entries_for_member(state.store.pool(), &member)
        .await
        .map_err(store_error)?;
    let entries: Vec<Value> = entries
        .into_iter()
        .map(|e| {
            json!({
                "seq": e.seq,
                "award_id": e.award_id,
                "delta": e.delta,
                "task_id": e.task_id,
                "approval_id": e.approval_id,
                "created_at": e.created_at,
            })
        })
        .collect();
    Ok(Json(
        json!({ "member": member, "balance": balance, "entries": entries }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::router;
    use crate::kernel::RecordingKernelPort;
    use crate::store::tasks::{self as store_tasks, ClaimOutcome, NewTask, SubmitOutcome};
    use crate::store::Cos72Store;
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    async fn get(router: &axum::Router, uri: &str) -> (StatusCode, Value) {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_str(std::str::from_utf8(&bytes).unwrap()).unwrap(),
        )
    }

    /// docs/agent/tasks.md T1.3.1 验收命令 #3
    /// `balance_equals_replay_of_ledger`: credit two awards to the same
    /// member via the real store path (submit → CAS → credit_award), then
    /// check `GET /points`/`GET /points/{member}` against a hand-summed
    /// replay.
    #[tokio::test]
    async fn balance_equals_replay_of_ledger() {
        let store = Cos72Store::open_memory().await.unwrap();
        let kernel = Arc::new(RecordingKernelPort::new());
        let state = Cos72State {
            capabilities: Arc::new(vec![]),
            store: store.clone(),
            kernel: kernel.clone(),
        };
        let router = router(state);

        for (i, points) in [(1, 10_i64), (2, 25)] {
            let task_id = format!("tsk_{i}");
            store_tasks::insert_task(
                store.pool(),
                NewTask {
                    task_id: &task_id,
                    title: "t",
                    description: "",
                    reward_points: points,
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
                &format!("awd_{i}"),
            )
            .await
            .unwrap();
            let SubmitOutcome::NeedsAdvise { award_id, .. } = outcome else {
                panic!("expected NeedsAdvise");
            };
            let approval_id = format!("appr_{i}");
            store_tasks::cas_write_approval_id(store.pool(), &award_id, &approval_id)
                .await
                .unwrap();
            crate::store::ledger::credit_award(
                store.pool(),
                &approval_id,
                "2026-01-01T00:00:03.000Z",
            )
            .await
            .unwrap();
        }

        let (status, body) = get(&router, "/points").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["entries"], 2);
        assert_eq!(body["balances"], json!([{"member": "mem1", "balance": 35}]));

        let (status, body) = get(&router, "/points/mem1").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["balance"], 35);
        assert_eq!(body["entries"].as_array().unwrap().len(), 2);

        let (status, body) = get(&router, "/points/nobody").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["balance"], 0);
        assert_eq!(body["entries"], json!([]));
    }

    /// docs/agent/tasks.md T1.3.1 验收命令 #3
    /// `restart_rebuilds_same_balances`: close the pool (drop it) and reopen
    /// the SAME on-disk file — the balance must be identical, since it is a
    /// pure replay of `points_ledger`, not anything cached.
    #[tokio::test]
    async fn restart_rebuilds_same_balances() {
        let dir = std::env::temp_dir().join(format!("cos72-ledger-restart-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("cos72.db");

        {
            let store = Cos72Store::open(&db_path).await.unwrap();
            store_tasks::insert_task(
                store.pool(),
                NewTask {
                    task_id: "tsk_1",
                    title: "t",
                    description: "",
                    reward_points: 42,
                    publisher: "pub1",
                    created_at: "2026-01-01T00:00:00.000Z",
                },
            )
            .await
            .unwrap();
            store_tasks::claim_task(store.pool(), "tsk_1", "mem1", "2026-01-01T00:00:01.000Z")
                .await
                .unwrap();
            let outcome = store_tasks::submit_task(
                store.pool(),
                "tsk_1",
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
            crate::store::ledger::credit_award(store.pool(), "appr-1", "2026-01-01T00:00:03.000Z")
                .await
                .unwrap();
        }

        let reopened = Cos72Store::open(&db_path).await.unwrap();
        let balance = ledger::balance_for_member(reopened.pool(), "mem1")
            .await
            .unwrap();
        assert_eq!(balance, 42, "balance must survive a close+reopen unchanged");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
