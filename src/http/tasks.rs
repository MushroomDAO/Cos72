//! mytask routes (docs/agent/spec.md「REST 路由」, docs/agent/tasks.md T1.2.1
//! 「开发范围」): publish/list/get/claim/submit. Only depends on `core`
//! (validation + the transition table), `store::tasks` (repository), and
//! `KernelPort` (events) — never touches sqlx or the SDK directly.
//!
//! T1.2.1 scope: submit only inserts a local `awaiting` award row with
//! `approval_id IS NULL` (docs/agent/tasks.md「明确不做」: advise/轮询/账
//! 本/记忆/`/points` are T1.3.1) — no `award.requested` event either (spec.md
//! ties that one to the advise call this task does not make).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::core::task::{self, TaskStatus};
use crate::http::error::{ApiError, ValidatedJson};
use crate::http::Cos72State;
use crate::kernel::KernelPort;
use crate::store::tasks::{
    self as store_tasks, AwardRow, ClaimOutcome, NewTask, SubmitOutcome, TaskRow,
};
use crate::store::StoreError;

fn now_rfc3339() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn store_error(err: StoreError) -> ApiError {
    tracing::error!(error = %err, "cos72: store error");
    ApiError::Internal
}

/// docs/agent/spec.md「REST 路由」task JSON shape, shared by every handler
/// below (`award` is only ever `Some` from [`get_task`] — the other handlers
/// have no reason to look one up).
fn task_json(row: &TaskRow, award: Option<&AwardRow>) -> Value {
    json!({
        "task_id": row.task_id,
        "title": row.title,
        "description": row.description,
        "reward_points": row.reward_points,
        "publisher": row.publisher,
        "claimer": row.claimer,
        "status": row.status,
        "evidence": row.evidence,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
        "award": award.map(|a| json!({
            "award_id": a.award_id,
            "state": a.state,
            "approval_id": a.approval_id,
        })),
    })
}

fn task_id_payload(task_id: &str) -> Map<String, Value> {
    let mut payload = Map::new();
    payload.insert("task_id".to_owned(), json!(task_id));
    payload
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishTaskRequest {
    title: String,
    #[serde(default)]
    description: Option<String>,
    reward_points: i64,
    publisher: String,
}

/// docs/agent/spec.md「REST 路由」`POST /tasks` → 201 `open`.
pub async fn publish_task<K: KernelPort>(
    State(state): State<Cos72State<K>>,
    ValidatedJson(req): ValidatedJson<PublishTaskRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if !task::is_valid_title(&req.title) {
        return Err(ApiError::InvalidRequest(
            "title must be 1..=200 non-blank characters".to_owned(),
        ));
    }
    let description = req.description.unwrap_or_default();
    if !task::is_valid_long_text(&description) {
        return Err(ApiError::InvalidRequest(
            "description must be at most 4000 characters".to_owned(),
        ));
    }
    if !task::is_valid_reward_points(req.reward_points) {
        return Err(ApiError::InvalidRequest(
            "reward_points must be an integer between 1 and 1,000,000".to_owned(),
        ));
    }
    if !task::is_valid_actor(&req.publisher) {
        return Err(ApiError::InvalidRequest(
            "publisher must match ^[a-z0-9][a-z0-9_-]{0,63}$".to_owned(),
        ));
    }

    let task_id = format!("tsk_{}", ulid::Ulid::new());
    let now = now_rfc3339();
    let row = store_tasks::insert_task(
        state.store.pool(),
        NewTask {
            task_id: &task_id,
            title: req.title.trim(),
            description: &description,
            reward_points: req.reward_points,
            publisher: &req.publisher,
            created_at: &now,
        },
    )
    .await
    .map_err(store_error)?;

    state
        .kernel
        .emit("task.published", task_id_payload(&row.task_id));

    Ok((StatusCode::CREATED, Json(task_json(&row, None))))
}

#[derive(Debug, Deserialize)]
pub struct ListTasksQuery {
    #[serde(default)]
    status: Option<String>,
}

/// docs/agent/spec.md「REST 路由」`GET /tasks?status=`: newest first, capped
/// at 200.
pub async fn list_tasks<K: KernelPort>(
    State(state): State<Cos72State<K>>,
    Query(params): Query<ListTasksQuery>,
) -> Result<Json<Value>, ApiError> {
    let status_filter = match &params.status {
        Some(s) => {
            if TaskStatus::parse(s).is_none() {
                return Err(ApiError::InvalidRequest(format!("unknown status: {s}")));
            }
            Some(s.as_str())
        }
        None => None,
    };

    let rows = store_tasks::list_tasks(state.store.pool(), status_filter)
        .await
        .map_err(store_error)?;
    let tasks: Vec<Value> = rows.iter().map(|row| task_json(row, None)).collect();
    Ok(Json(json!({ "tasks": tasks })))
}

/// docs/agent/spec.md「REST 路由」`GET /tasks/{id}`: task + current award
/// summary.
pub async fn get_task<K: KernelPort>(
    State(state): State<Cos72State<K>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row = store_tasks::get_task(state.store.pool(), &id)
        .await
        .map_err(store_error)?
        .ok_or(ApiError::NotFound)?;
    let award = store_tasks::latest_award_for_task(state.store.pool(), &id)
        .await
        .map_err(store_error)?;
    Ok(Json(task_json(&row, award.as_ref())))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRequest {
    member: String,
}

/// docs/agent/spec.md「状态机」`open → claimed`.
pub async fn claim_task<K: KernelPort>(
    State(state): State<Cos72State<K>>,
    Path(id): Path<String>,
    ValidatedJson(req): ValidatedJson<ClaimRequest>,
) -> Result<Json<Value>, ApiError> {
    if !task::is_valid_actor(&req.member) {
        return Err(ApiError::InvalidRequest(
            "member must match ^[a-z0-9][a-z0-9_-]{0,63}$".to_owned(),
        ));
    }
    let now = now_rfc3339();
    match store_tasks::claim_task(state.store.pool(), &id, &req.member, &now)
        .await
        .map_err(store_error)?
    {
        ClaimOutcome::Claimed(row) => {
            state
                .kernel
                .emit("task.claimed", task_id_payload(&row.task_id));
            Ok(Json(task_json(&row, None)))
        }
        ClaimOutcome::NotFound => Err(ApiError::NotFound),
        ClaimOutcome::InvalidTransition => Err(ApiError::InvalidTransition),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitRequest {
    member: String,
    #[serde(default)]
    evidence: Option<String>,
}

/// docs/agent/spec.md「状态机」`claimed → submitted` — T1.2.1 scope only:
/// inserts the local `awaiting` award row (`approval_id IS NULL`), does NOT
/// call `advise` (T1.3.1).
pub async fn submit_task<K: KernelPort>(
    State(state): State<Cos72State<K>>,
    Path(id): Path<String>,
    ValidatedJson(req): ValidatedJson<SubmitRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if !task::is_valid_actor(&req.member) {
        return Err(ApiError::InvalidRequest(
            "member must match ^[a-z0-9][a-z0-9_-]{0,63}$".to_owned(),
        ));
    }
    if let Some(evidence) = &req.evidence {
        if !task::is_valid_long_text(evidence) {
            return Err(ApiError::InvalidRequest(
                "evidence must be at most 4000 characters".to_owned(),
            ));
        }
    }

    let now = now_rfc3339();
    let award_id = format!("awd_{}", ulid::Ulid::new());
    match store_tasks::submit_task(
        state.store.pool(),
        &id,
        &req.member,
        req.evidence.as_deref(),
        &now,
        &award_id,
    )
    .await
    .map_err(store_error)?
    {
        SubmitOutcome::Submitted { task, award_id } => {
            state
                .kernel
                .emit("task.submitted", task_id_payload(&task.task_id));
            let body = json!({
                "task": task_json(&task, None),
                "award": {
                    "award_id": award_id,
                    "approval_id": Value::Null,
                    "state": "awaiting",
                },
            });
            Ok((StatusCode::ACCEPTED, Json(body)))
        }
        SubmitOutcome::NotFound => Err(ApiError::NotFound),
        SubmitOutcome::InvalidTransition => Err(ApiError::InvalidTransition),
        SubmitOutcome::NotClaimer => Err(ApiError::NotClaimer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::router;
    use crate::kernel::RecordingKernelPort;
    use crate::store::Cos72Store;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use axum::Router;
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    async fn test_router() -> (Router, Arc<RecordingKernelPort>, Cos72Store) {
        let kernel = Arc::new(RecordingKernelPort::new());
        let store = Cos72Store::open_memory().await.unwrap();
        let state = Cos72State {
            capabilities: Arc::new(vec![]),
            store: store.clone(),
            kernel: kernel.clone(),
        };
        (router(state), kernel, store)
    }

    async fn call(
        router: &Router,
        method: Method,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(match body {
                Some(v) => Body::from(v.to_string()),
                None => Body::empty(),
            })
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(std::str::from_utf8(&bytes).unwrap()).unwrap()
        };
        (status, value)
    }

    fn publish_body(points: i64) -> Value {
        json!({"title": "t", "reward_points": points, "publisher": "pub1"})
    }

    async fn publish(router: &Router) -> String {
        let (status, body) = call(router, Method::POST, "/tasks", Some(publish_body(10))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["task_id"].as_str().unwrap().to_owned()
    }

    /// docs/agent/tasks.md T1.2.1 验收命令 #2.
    #[tokio::test]
    async fn publish_returns_201_open() {
        let (router, _kernel, _store) = test_router().await;
        let (status, body) = call(&router, Method::POST, "/tasks", Some(publish_body(10))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["status"], "open");
        assert_eq!(body["claimer"], Value::Null);
        assert!(
            body["task_id"].as_str().unwrap().starts_with("tsk_"),
            "{body}"
        );
    }

    /// 正对照 embedded in the same test (docs/agent/tasks.md T1.2.1 验收命
    /// 令 #2): 1 and 1,000,000 succeed in the same run that proves 0 and
    /// 1,000,001 fail.
    #[tokio::test]
    async fn publish_points_bounds() {
        let (router, _kernel, _store) = test_router().await;

        let (s_zero, b_zero) = call(&router, Method::POST, "/tasks", Some(publish_body(0))).await;
        assert_eq!(s_zero, StatusCode::BAD_REQUEST, "{b_zero}");

        let (s_over, b_over) = call(
            &router,
            Method::POST,
            "/tasks",
            Some(publish_body(1_000_001)),
        )
        .await;
        assert_eq!(s_over, StatusCode::BAD_REQUEST, "{b_over}");

        let (s_one, b_one) = call(&router, Method::POST, "/tasks", Some(publish_body(1))).await;
        assert_eq!(s_one, StatusCode::CREATED, "{b_one}");

        let (s_max, b_max) = call(
            &router,
            Method::POST,
            "/tasks",
            Some(publish_body(1_000_000)),
        )
        .await;
        assert_eq!(s_max, StatusCode::CREATED, "{b_max}");
    }

    /// docs/agent/tasks.md T1.2.1 验收命令 #2.
    #[tokio::test]
    async fn claim_twice_is_409() {
        let (router, _kernel, _store) = test_router().await;
        let task_id = publish(&router).await;

        let (s1, b1) = call(
            &router,
            Method::POST,
            &format!("/tasks/{task_id}/claim"),
            Some(json!({"member": "mem1"})),
        )
        .await;
        assert_eq!(s1, StatusCode::OK, "{b1}");

        let (s2, b2) = call(
            &router,
            Method::POST,
            &format!("/tasks/{task_id}/claim"),
            Some(json!({"member": "mem2"})),
        )
        .await;
        assert_eq!(s2, StatusCode::CONFLICT, "{b2}");
        assert_eq!(b2["error"]["code"], "invalid_transition");
    }

    /// docs/agent/tasks.md T1.2.1 验收命令 #2, 正对照 in the same test: the
    /// actual claimer's submit succeeds with 202.
    #[tokio::test]
    async fn submit_by_non_claimer_is_403() {
        let (router, _kernel, _store) = test_router().await;
        let task_id = publish(&router).await;
        call(
            &router,
            Method::POST,
            &format!("/tasks/{task_id}/claim"),
            Some(json!({"member": "mem1"})),
        )
        .await;

        let (s_wrong, b_wrong) = call(
            &router,
            Method::POST,
            &format!("/tasks/{task_id}/submit"),
            Some(json!({"member": "mem2"})),
        )
        .await;
        assert_eq!(s_wrong, StatusCode::FORBIDDEN, "{b_wrong}");
        assert_eq!(b_wrong["error"]["code"], "not_claimer");

        let (s_ok, b_ok) = call(
            &router,
            Method::POST,
            &format!("/tasks/{task_id}/submit"),
            Some(json!({"member": "mem1"})),
        )
        .await;
        assert_eq!(s_ok, StatusCode::ACCEPTED, "{b_ok}");
        assert_eq!(b_ok["task"]["status"], "submitted");
        assert_eq!(b_ok["award"]["approval_id"], Value::Null);
        assert_eq!(b_ok["award"]["state"], "awaiting");
    }

    /// docs/agent/tasks.md T1.2.1 验收命令 #2 — reaches into the store
    /// directly (same crate, `Cos72Store::pool()` is `pub(crate)`) to assert
    /// the row shape spec.md requires, not just the HTTP response body.
    #[tokio::test]
    async fn submit_inserts_exactly_one_awaiting_award_with_null_approval_id() {
        let (router, _kernel, store) = test_router().await;
        let task_id = publish(&router).await;
        call(
            &router,
            Method::POST,
            &format!("/tasks/{task_id}/claim"),
            Some(json!({"member": "mem1"})),
        )
        .await;
        let (status, body) = call(
            &router,
            Method::POST,
            &format!("/tasks/{task_id}/submit"),
            Some(json!({"member": "mem1"})),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");

        let rows: Vec<(String, Option<String>, String)> =
            sqlx::query_as("SELECT award_id, approval_id, state FROM awards WHERE task_id = ?")
                .bind(&task_id)
                .fetch_all(store.pool())
                .await
                .unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].1, None, "{rows:?}");
        assert_eq!(rows[0].2, "awaiting", "{rows:?}");
    }

    /// docs/agent/tasks.md T1.2.1 验收命令 #2
    /// `concurrent_claims_exactly_one_wins`: 10 concurrent claims on the same
    /// `open` task → exactly one 200, nine 409 — the store's CAS `UPDATE ...
    /// WHERE status = 'open'` is the mechanism under test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_claims_exactly_one_wins() {
        let (router, _kernel, _store) = test_router().await;
        let task_id = publish(&router).await;

        let mut handles = Vec::new();
        for i in 0..10 {
            let router = router.clone();
            let task_id = task_id.clone();
            handles.push(tokio::spawn(async move {
                call(
                    &router,
                    Method::POST,
                    &format!("/tasks/{task_id}/claim"),
                    Some(json!({"member": format!("mem{i}")})),
                )
                .await
            }));
        }

        let mut ok = 0;
        let mut conflict = 0;
        for handle in handles {
            let (status, body) = handle.await.unwrap();
            match status {
                StatusCode::OK => ok += 1,
                StatusCode::CONFLICT => conflict += 1,
                other => panic!("unexpected status {other}: {body}"),
            }
        }
        assert_eq!(ok, 1, "exactly one claim must win");
        assert_eq!(conflict, 9, "the other nine must see 409");
    }

    /// docs/agent/tasks.md T1.2.1 验收命令 #2 — every request struct is
    /// `#[serde(deny_unknown_fields)]`.
    #[tokio::test]
    async fn unknown_body_fields_rejected() {
        let (router, _kernel, _store) = test_router().await;
        let (status, body) = call(
            &router,
            Method::POST,
            "/tasks",
            Some(json!({
                "title": "t",
                "reward_points": 10,
                "publisher": "pub1",
                "unexpected_field": true,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "invalid_request");
    }

    /// docs/agent/tasks.md T1.2.1 验收命令 #3: `cargo test events::` — one
    /// event per successful transition, none on a failed one.
    mod events {
        use super::*;

        #[tokio::test]
        async fn each_transition_emits_one_event_carrying_task_id() {
            let (router, kernel, _store) = test_router().await;

            let (s_pub, b_pub) =
                call(&router, Method::POST, "/tasks", Some(publish_body(10))).await;
            assert_eq!(s_pub, StatusCode::CREATED, "{b_pub}");
            let task_id = b_pub["task_id"].as_str().unwrap().to_owned();

            let (s_claim, b_claim) = call(
                &router,
                Method::POST,
                &format!("/tasks/{task_id}/claim"),
                Some(json!({"member": "mem1"})),
            )
            .await;
            assert_eq!(s_claim, StatusCode::OK, "{b_claim}");

            let (s_submit, b_submit) = call(
                &router,
                Method::POST,
                &format!("/tasks/{task_id}/submit"),
                Some(json!({"member": "mem1"})),
            )
            .await;
            assert_eq!(s_submit, StatusCode::ACCEPTED, "{b_submit}");

            let emitted = kernel.emitted();
            assert_eq!(emitted.len(), 3, "{emitted:?}");
            assert_eq!(emitted[0].0, "task.published");
            assert_eq!(emitted[0].1["task_id"], json!(task_id));
            assert_eq!(emitted[1].0, "task.claimed");
            assert_eq!(emitted[1].1["task_id"], json!(task_id));
            assert_eq!(emitted[2].0, "task.submitted");
            assert_eq!(emitted[2].1["task_id"], json!(task_id));

            // 正对照: a failing (409) transition must NOT add a fourth event.
            let (s_again, b_again) = call(
                &router,
                Method::POST,
                &format!("/tasks/{task_id}/claim"),
                Some(json!({"member": "mem2"})),
            )
            .await;
            assert_eq!(s_again, StatusCode::CONFLICT, "{b_again}");
            assert_eq!(
                kernel.emitted().len(),
                3,
                "a failed transition must not emit an event"
            );
        }
    }
}
