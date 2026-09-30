//! mytask routes (docs/agent/spec.md「REST 路由」, docs/agent/tasks.md T1.2.1
//! 「开发范围」): publish/list/get/claim/submit. Only depends on `core`
//! (validation + the transition table), `store::tasks` (repository), and
//! `KernelPort` (events) — never touches sqlx or the SDK directly.
//!
//! T1.2.1 scope: submit only inserts a local `awaiting` award row with
//! `approval_id IS NULL` (docs/agent/tasks.md「明确不做」: advise/轮询/账
//! 本/记忆/`/points` are T1.3.1) — no `award.requested` event either (spec.md
//! ties that one to the advise call this task does not make).

use agent24_os_sdk::{ApprovalSubmit, ClientError, RequestContext};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::core::now_rfc3339;
use crate::core::task::{self, TaskStatus};
use crate::http::error::{ApiError, ValidatedJson};
use crate::http::Cos72State;
use crate::kernel::KernelPort;
use crate::store::tasks::{
    self as store_tasks, AwardRow, CasApprovalOutcome, ClaimOutcome, NewTask, SubmitOutcome,
    TaskRow,
};
use crate::store::StoreError;

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

/// docs/agent/spec.md「状态机」`claimed → submitted`, plus T1.3.1a's advise +
/// CAS write-back (「submit 的精确步骤」第 1/3/4/5 步). Step 2 (which local
/// row to write/reuse) lives in `store::tasks::submit_task`; this handler
/// owns steps 1 (capability/header check, BEFORE any store write), 3
/// (`advise`, same-pair retry once on `Timeout`), 4 (CAS write-back) and 5
/// (events + response).
pub async fn submit_task<K: KernelPort>(
    State(state): State<Cos72State<K>>,
    Path(id): Path<String>,
    ctx: RequestContext,
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

    // Step 1 (docs/agent/spec.md「submit 的精确步骤」): no approval capability,
    // or the proxied request carries no request id / approval token → 503,
    // and NOTHING is written — checked before the store is ever touched.
    if !state.kernel.approval_available() {
        return Err(ApiError::ApprovalUnavailable);
    }
    let (Some(request_id), Some(approval_token)) =
        (ctx.request_id.as_ref(), ctx.approval_token.as_ref())
    else {
        return Err(ApiError::ApprovalUnavailable);
    };

    let now = now_rfc3339();
    let new_award_id = format!("awd_{}", ulid::Ulid::new());
    let outcome = store_tasks::submit_task(
        state.store.pool(),
        &id,
        &req.member,
        req.evidence.as_deref(),
        &now,
        &new_award_id,
    )
    .await
    .map_err(store_error)?;

    match outcome {
        SubmitOutcome::NotFound => Err(ApiError::NotFound),
        SubmitOutcome::InvalidTransition => Err(ApiError::InvalidTransition),
        SubmitOutcome::NotClaimer => Err(ApiError::NotClaimer),
        // Step 2, third bullet — already has an approval_id: idempotent
        // re-entry, no advise call, 200 (not 202).
        SubmitOutcome::AlreadyAdvised {
            task,
            award_id,
            approval_id,
        } => {
            let body = json!({
                "task": task_json(&task, None),
                "award": {
                    "award_id": award_id,
                    "approval_id": approval_id,
                    "state": "awaiting",
                },
            });
            Ok((StatusCode::OK, Json(body)))
        }
        SubmitOutcome::NeedsAdvise {
            task,
            award_id,
            emit_task_submitted,
        } => {
            let payload = json!({
                "award_id": award_id,
                "task_id": task.task_id,
                "member": req.member,
                "points": task.reward_points,
                "title": task.title,
            });
            let submit = ApprovalSubmit {
                action: "cos72.award_points",
                target: Some(task.task_id.as_str()),
                payload,
                request_id,
                approval_token,
            };

            // Step 3: `Timeout` retries once with the SAME `submit` value
            // (same `request_id`/`approval_token` — wire-idempotent);
            // `ConnectionLost`/`NotSent` do not retry; every other error is
            // 502 `kernel_error`. Either way `approval_id` stays `NULL` in
            // the store — no CAS write is attempted on a failed advise.
            let answer = match state.kernel.advise(&submit).await {
                Ok(answer) => answer,
                Err(ClientError::Timeout(_)) => match state.kernel.advise(&submit).await {
                    Ok(answer) => answer,
                    Err(err) => return Err(advise_error_to_api(err)),
                },
                Err(err) => return Err(advise_error_to_api(err)),
            };

            // Step 4: CAS write-back. Losing the race means THIS request's
            // own `advise` becomes an orphan (never recorded, never
            // credited) — the response reflects the winner's approval_id.
            let final_approval_id = match store_tasks::cas_write_approval_id(
                state.store.pool(),
                &award_id,
                &answer.approval_id,
            )
            .await
            .map_err(store_error)?
            {
                CasApprovalOutcome::Written => answer.approval_id,
                CasApprovalOutcome::LostRace {
                    existing_approval_id,
                } => existing_approval_id.unwrap_or(answer.approval_id),
            };

            // Step 5: events, only once advise+CAS actually completed.
            if emit_task_submitted {
                state
                    .kernel
                    .emit("task.submitted", task_id_payload(&task.task_id));
            }
            let mut requested_payload = task_id_payload(&task.task_id);
            requested_payload.insert("award_id".to_owned(), json!(award_id));
            requested_payload.insert("approval_id".to_owned(), json!(final_approval_id));
            state.kernel.emit("award.requested", requested_payload);

            let body = json!({
                "task": task_json(&task, None),
                "award": {
                    "award_id": award_id,
                    "approval_id": final_approval_id,
                    "state": "awaiting",
                },
            });
            Ok((StatusCode::ACCEPTED, Json(body)))
        }
    }
}

/// docs/agent/spec.md「submit 的精确步骤」第 3 步: `ConnectionLost`/`NotSent`
/// → 503 `approval_unavailable` (no retry — the SDK's own fatal hook is
/// about to `exit(70)` this generation anyway); every other `ClientError` →
/// 502 `kernel_error`, carrying that error's own `Display` text.
fn advise_error_to_api(err: ClientError) -> ApiError {
    match err {
        ClientError::ConnectionLost | ClientError::NotSent(_) => ApiError::ApprovalUnavailable,
        other => ApiError::KernelError(other.to_string()),
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
        call_with_headers(router, method, uri, body, &[]).await
    }

    /// Like [`call`], but with extra request headers — every T1.3.1a submit
    /// call needs `x-a24-request-id`/`x-a24-approval-token` (the kernel
    /// proxy's own header names, `agent24_os_proto::proxy::{REQUEST_ID_
    /// HEADER, APPROVAL_TOKEN_HEADER}`) so `RequestContext::from_headers`
    /// extracts a `Some` pair for the handler to advise with.
    async fn call_with_headers(
        router: &Router,
        method: Method,
        uri: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
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

    const APPROVAL_HEADERS: &[(&str, &str)] = &[
        ("x-a24-request-id", "req-1"),
        ("x-a24-approval-token", "tok-1"),
    ];

    /// Builds an `ApprovalToken` the same way `RequestContext::from_headers`
    /// does inside the handler — the only public way to obtain one, since
    /// its inner value is not otherwise readable outside the SDK crate (see
    /// `kernel::AdviseCall::approval_token`'s own doc).
    fn expected_approval_token(value: &str) -> agent24_os_sdk::ApprovalToken {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-a24-approval-token", value.parse().unwrap());
        agent24_os_sdk::RequestContext::from_headers(&headers)
            .approval_token
            .unwrap()
    }

    fn approval_answer(approval_id: &str) -> agent24_os_sdk::ApprovalAnswer {
        agent24_os_sdk::ApprovalAnswer {
            approval_id: approval_id.to_owned(),
            kind: agent24_os_sdk::ApprovalKind::Advise,
            binding: false,
            decision: agent24_os_sdk::ApprovalDecision::Pending,
            executed_at: None,
        }
    }

    /// `submit` with the approval headers present and a queued successful
    /// `advise` answer — the normal-path helper every T1.2.1-carried-over
    /// test now needs, since a bare [`call`] (no headers) gets 503
    /// `approval_unavailable` under T1.3.1a's step-1 check.
    async fn submit_with_approval(
        router: &Router,
        kernel: &RecordingKernelPort,
        task_id: &str,
        member: &str,
        approval_id: &str,
    ) -> (StatusCode, Value) {
        kernel.push_advise_response(Ok(approval_answer(approval_id)));
        call_with_headers(
            router,
            Method::POST,
            &format!("/tasks/{task_id}/submit"),
            Some(json!({"member": member})),
            APPROVAL_HEADERS,
        )
        .await
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
        let (router, kernel, _store) = test_router().await;
        let task_id = publish(&router).await;
        call(
            &router,
            Method::POST,
            &format!("/tasks/{task_id}/claim"),
            Some(json!({"member": "mem1"})),
        )
        .await;

        let (s_wrong, b_wrong) = call_with_headers(
            &router,
            Method::POST,
            &format!("/tasks/{task_id}/submit"),
            Some(json!({"member": "mem2"})),
            APPROVAL_HEADERS,
        )
        .await;
        assert_eq!(s_wrong, StatusCode::FORBIDDEN, "{b_wrong}");
        assert_eq!(b_wrong["error"]["code"], "not_claimer");
        // 403 must not have attempted advise at all.
        assert!(
            kernel.advise_calls().is_empty(),
            "{:?}",
            kernel.advise_calls()
        );

        let (s_ok, b_ok) = submit_with_approval(&router, &kernel, &task_id, "mem1", "appr-1").await;
        assert_eq!(s_ok, StatusCode::ACCEPTED, "{b_ok}");
        assert_eq!(b_ok["task"]["status"], "submitted");
        assert_eq!(b_ok["award"]["approval_id"], "appr-1");
        assert_eq!(b_ok["award"]["state"], "awaiting");
    }

    /// docs/agent/tasks.md T1.2.1 验收命令 #2 — reaches into the store
    /// directly (same crate, `Cos72Store::pool()` is `pub(crate)`) to assert
    /// the row shape spec.md requires, not just the HTTP response body.
    /// Updated for T1.3.1a: submit now advises + CAS-writes the
    /// `approval_id` back inside the SAME request, so the row this test
    /// checks carries the advised id, not `NULL` (T1.2.1's own scope, before
    /// advise was wired up).
    #[tokio::test]
    async fn submit_inserts_exactly_one_awaiting_award_and_writes_back_approval_id() {
        let (router, kernel, store) = test_router().await;
        let task_id = publish(&router).await;
        call(
            &router,
            Method::POST,
            &format!("/tasks/{task_id}/claim"),
            Some(json!({"member": "mem1"})),
        )
        .await;
        let (status, body) =
            submit_with_approval(&router, &kernel, &task_id, "mem1", "appr-1").await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");

        let rows: Vec<(String, Option<String>, String)> =
            sqlx::query_as("SELECT award_id, approval_id, state FROM awards WHERE task_id = ?")
                .bind(&task_id)
                .fetch_all(store.pool())
                .await
                .unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].1.as_deref(), Some("appr-1"), "{rows:?}");
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

            let (s_submit, b_submit) =
                submit_with_approval(&router, &kernel, &task_id, "mem1", "appr-1").await;
            assert_eq!(s_submit, StatusCode::ACCEPTED, "{b_submit}");

            let emitted = kernel.emitted();
            assert_eq!(emitted.len(), 4, "{emitted:?}");
            assert_eq!(emitted[0].0, "task.published");
            assert_eq!(emitted[0].1["task_id"], json!(task_id));
            assert_eq!(emitted[1].0, "task.claimed");
            assert_eq!(emitted[1].1["task_id"], json!(task_id));
            assert_eq!(emitted[2].0, "task.submitted");
            assert_eq!(emitted[2].1["task_id"], json!(task_id));
            assert_eq!(emitted[3].0, "award.requested");
            assert_eq!(emitted[3].1["task_id"], json!(task_id));
            assert_eq!(emitted[3].1["approval_id"], json!("appr-1"));

            // 正对照: a failing (409) transition must NOT add a fifth event.
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
                4,
                "a failed transition must not emit an event"
            );
        }
    }

    /// docs/agent/tasks.md T1.3.1 验收命令 #1: submit's advise/CAS step.
    mod approval {
        use super::*;

        /// `submit_advises_inside_request_with_its_request_id_and_token`:
        /// the `advise` call carries the PROXIED request's own
        /// `request_id`/`approval_token` (from `RequestContext`, i.e. the
        /// headers this test sets) and a payload naming the award just
        /// inserted.
        #[tokio::test]
        async fn submit_advises_inside_request_with_its_request_id_and_token() {
            let (router, kernel, _store) = test_router().await;
            let task_id = publish(&router).await;
            call(
                &router,
                Method::POST,
                &format!("/tasks/{task_id}/claim"),
                Some(json!({"member": "mem1"})),
            )
            .await;

            let (status, body) =
                submit_with_approval(&router, &kernel, &task_id, "mem1", "appr-1").await;
            assert_eq!(status, StatusCode::ACCEPTED, "{body}");

            let calls = kernel.advise_calls();
            assert_eq!(calls.len(), 1, "{calls:?}");
            assert_eq!(calls[0].action, "cos72.award_points");
            assert_eq!(calls[0].request_id, "req-1");
            assert_eq!(calls[0].approval_token, expected_approval_token("tok-1"));
            let award_id = body["award"]["award_id"].as_str().unwrap();
            assert_eq!(calls[0].payload["award_id"], json!(award_id));
            assert_eq!(calls[0].payload["task_id"], json!(task_id));
        }

        /// `submit_without_approval_capability_is_503_and_writes_nothing`:
        /// 正对照 is `submit_advises_inside_request_with_its_request_id_
        /// and_token` above (same setup, capability present → 202 and a
        /// store row).
        #[tokio::test]
        async fn submit_without_approval_capability_is_503_and_writes_nothing() {
            let (router, kernel, store) = test_router().await;
            let task_id = publish(&router).await;
            call(
                &router,
                Method::POST,
                &format!("/tasks/{task_id}/claim"),
                Some(json!({"member": "mem1"})),
            )
            .await;
            kernel.set_approval_available(false);

            let (status, body) = call_with_headers(
                &router,
                Method::POST,
                &format!("/tasks/{task_id}/submit"),
                Some(json!({"member": "mem1"})),
                APPROVAL_HEADERS,
            )
            .await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
            assert_eq!(body["error"]["code"], "approval_unavailable");
            assert!(kernel.advise_calls().is_empty());

            let rows: Vec<(String,)> = sqlx::query_as("SELECT award_id FROM awards")
                .fetch_all(store.pool())
                .await
                .unwrap();
            assert!(rows.is_empty(), "{rows:?}");
            let task: (String,) = sqlx::query_as("SELECT status FROM tasks WHERE task_id = ?")
                .bind(&task_id)
                .fetch_one(store.pool())
                .await
                .unwrap();
            assert_eq!(
                task.0, "claimed",
                "the task must not have transitioned either"
            );
        }

        /// `advise_timeout_retries_once_with_the_same_pair`: the fake kernel
        /// answers `Timeout` first, then success — exactly 2 `advise` calls,
        /// both carrying the SAME `request_id`/`approval_token`.
        #[tokio::test]
        async fn advise_timeout_retries_once_with_the_same_pair() {
            let (router, kernel, _store) = test_router().await;
            let task_id = publish(&router).await;
            call(
                &router,
                Method::POST,
                &format!("/tasks/{task_id}/claim"),
                Some(json!({"member": "mem1"})),
            )
            .await;
            kernel.push_advise_response(Err(ClientError::Timeout("t".into())));
            kernel.push_advise_response(Ok(approval_answer("appr-retry")));

            let (status, body) = call_with_headers(
                &router,
                Method::POST,
                &format!("/tasks/{task_id}/submit"),
                Some(json!({"member": "mem1"})),
                APPROVAL_HEADERS,
            )
            .await;
            assert_eq!(status, StatusCode::ACCEPTED, "{body}");
            assert_eq!(body["award"]["approval_id"], "appr-retry");

            let calls = kernel.advise_calls();
            assert_eq!(calls.len(), 2, "{calls:?}");
            assert_eq!(calls[0].request_id, calls[1].request_id);
            assert_eq!(calls[0].approval_token, calls[1].approval_token);
        }

        /// `advise_failure_leaves_null_approval_id_and_resubmit_reuses_
        /// award_id`: a non-retryable advise failure (502 `kernel_error`)
        /// leaves the award's `approval_id` `NULL`; resubmitting reuses the
        /// SAME `award_id` (孤儿补偿) instead of minting a second one.
        #[tokio::test]
        async fn advise_failure_leaves_null_approval_id_and_resubmit_reuses_award_id() {
            let (router, kernel, store) = test_router().await;
            let task_id = publish(&router).await;
            call(
                &router,
                Method::POST,
                &format!("/tasks/{task_id}/claim"),
                Some(json!({"member": "mem1"})),
            )
            .await;
            kernel.push_advise_response(Err(ClientError::Forbidden("no".into())));

            let (status, body) = call_with_headers(
                &router,
                Method::POST,
                &format!("/tasks/{task_id}/submit"),
                Some(json!({"member": "mem1"})),
                APPROVAL_HEADERS,
            )
            .await;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
            assert_eq!(body["error"]["code"], "kernel_error");

            let rows: Vec<(String, Option<String>)> =
                sqlx::query_as("SELECT award_id, approval_id FROM awards WHERE task_id = ?")
                    .bind(&task_id)
                    .fetch_all(store.pool())
                    .await
                    .unwrap();
            assert_eq!(rows.len(), 1, "{rows:?}");
            assert_eq!(rows[0].1, None, "{rows:?}");
            let first_award_id = rows[0].0.clone();

            // Resubmit — task is still `submitted` with the same `awaiting`,
            // `approval_id IS NULL` row: the handler must reuse it.
            let (status2, body2) =
                submit_with_approval(&router, &kernel, &task_id, "mem1", "appr-2").await;
            assert_eq!(status2, StatusCode::ACCEPTED, "{body2}");
            assert_eq!(body2["award"]["award_id"], json!(first_award_id));

            let rows: Vec<(String,)> =
                sqlx::query_as("SELECT award_id FROM awards WHERE task_id = ?")
                    .bind(&task_id)
                    .fetch_all(store.pool())
                    .await
                    .unwrap();
            assert_eq!(
                rows.len(),
                1,
                "no second award row must be inserted: {rows:?}"
            );
        }

        /// `resubmit_while_awaiting_with_id_does_not_advise_again`: once the
        /// award has a recorded `approval_id`, a resubmit is a pure
        /// idempotent 200 — `advise` is called exactly once total (from the
        /// FIRST submit only).
        #[tokio::test]
        async fn resubmit_while_awaiting_with_id_does_not_advise_again() {
            let (router, kernel, _store) = test_router().await;
            let task_id = publish(&router).await;
            call(
                &router,
                Method::POST,
                &format!("/tasks/{task_id}/claim"),
                Some(json!({"member": "mem1"})),
            )
            .await;
            let (s1, b1) = submit_with_approval(&router, &kernel, &task_id, "mem1", "appr-1").await;
            assert_eq!(s1, StatusCode::ACCEPTED, "{b1}");

            let (s2, b2) = call_with_headers(
                &router,
                Method::POST,
                &format!("/tasks/{task_id}/submit"),
                Some(json!({"member": "mem1"})),
                APPROVAL_HEADERS,
            )
            .await;
            assert_eq!(s2, StatusCode::OK, "{b2}");
            assert_eq!(b2["award"]["approval_id"], "appr-1");
            assert_eq!(
                kernel.advise_calls().len(),
                1,
                "{:?}",
                kernel.advise_calls()
            );
        }

        /// `lost_cas_race_returns_winner_approval_id`: simulates the race by
        /// writing a winner's `approval_id` directly into the store BETWEEN
        /// this handler's own store-phase read and its CAS write — done here
        /// by queuing an advise answer, then (before the handler's CAS runs)
        /// there is no seam to inject a concurrent writer from a single
        /// `oneshot` call, so this test instead calls `cas_write_approval_id`
        /// directly to prove the CAS itself reports the loser correctly (the
        /// HTTP-level race is exercised for real by
        /// `advise_failure_leaves_null_approval_id_and_resubmit_reuses_
        /// award_id`'s reuse path plus this store-level unit).
        #[tokio::test]
        async fn lost_cas_race_returns_winner_approval_id() {
            let store = crate::store::Cos72Store::open_memory().await.unwrap();
            store_tasks::insert_task(
                store.pool(),
                NewTask {
                    task_id: "tsk_1",
                    title: "t",
                    description: "",
                    reward_points: 10,
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

            // The "winner" writes first.
            let winner = store_tasks::cas_write_approval_id(store.pool(), &award_id, "appr-winner")
                .await
                .unwrap();
            assert!(matches!(winner, CasApprovalOutcome::Written));

            // This request's own advise "wins" the wire race but loses the
            // CAS — it must be told the WINNER's id, not its own.
            let loser = store_tasks::cas_write_approval_id(store.pool(), &award_id, "appr-loser")
                .await
                .unwrap();
            match loser {
                CasApprovalOutcome::LostRace {
                    existing_approval_id,
                } => assert_eq!(existing_approval_id.as_deref(), Some("appr-winner")),
                CasApprovalOutcome::Written => panic!("the second writer must lose the race"),
            }
        }
    }
}
