//! `test-hooks`-only debug routes (docs/agent/architecture.md 不可动摇的边界
//! #9: "`/debug/*` 路由只在 `test-hooks` feature 下编译，正式包不带"). The whole
//! module is behind that feature (`http::router` only registers
//! `/debug/memory-recall` when it is enabled) — never present in a real
//! package build.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::http::error::ApiError;
use crate::http::Cos72State;
use crate::kernel::KernelPort;

/// spec.md「记忆泵」/「REST 路由」: page size for the debug recall query — a
/// fixed, generous default; this route exists purely for a human/black-box
/// test to eyeball what Cos72 actually wrote, not for production paging.
const DEBUG_RECALL_PAGE_SIZE: usize = 50;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRecallRequest {
    query: String,
}

/// docs/agent/spec.md「REST 路由」`POST /debug/memory-recall`（仅
/// `test-hooks`）: `{query}` → `{items: [...]}` — a thin pass-through to the
/// kernel's own `recall`, so a black-box test can verify a dedup key really
/// landed in Cos72's private memory (and, in the coexistence test T1.4.1
/// adds, that it canNOT see another module's memories).
pub async fn memory_recall<K: KernelPort>(
    State(state): State<Cos72State<K>>,
    axum::Json(req): axum::Json<MemoryRecallRequest>,
) -> Result<Json<Value>, ApiError> {
    let page = state
        .kernel
        .recall(&req.query, DEBUG_RECALL_PAGE_SIZE)
        .await
        .map_err(|err| ApiError::KernelError(err.to_string()))?;
    let items: Vec<Value> = page
        .items
        .into_iter()
        .map(|item| {
            json!({
                "id": item.id,
                "kind": item.kind,
                "body": item.body,
                "at": item.at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::router;
    use crate::kernel::RecordingKernelPort;
    use crate::store::Cos72Store;
    use agent24_os_sdk::{ClientError, RecallPage, Recollection};
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::Map;
    use std::sync::Arc;
    use tower::ServiceExt;

    async fn post(router: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
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

    /// Wiring smoke test: the route exists under `test-hooks`, forwards the
    /// query to `KernelPort::recall`, and shapes the response as
    /// `{items: [...]}`.
    #[tokio::test]
    async fn memory_recall_forwards_query_and_shapes_items() {
        let kernel = Arc::new(RecordingKernelPort::new());
        let mut body = Map::new();
        body.insert("dedup_key".to_owned(), json!("cos72:task:tsk_1:completed"));
        kernel.push_recall_response(Ok(RecallPage {
            items: vec![Recollection {
                id: "osmem:1".to_owned(),
                kind: "task.summary".to_owned(),
                body,
                at: "2026-01-01T00:00:00.000Z".to_owned(),
            }],
            cursor: None,
        }));
        let state = Cos72State {
            capabilities: Arc::new(vec![]),
            store: Cos72Store::open_memory().await.unwrap(),
            kernel: kernel.clone(),
        };
        let router = router(state);

        let (status, body) = post(
            &router,
            "/debug/memory-recall",
            json!({"query": "cos72:task:tsk_1:completed"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["items"].as_array().unwrap().len(), 1);
        assert_eq!(body["items"][0]["id"], "osmem:1");
        assert_eq!(
            kernel.recall_calls(),
            vec![("cos72:task:tsk_1:completed".to_owned(), 50)]
        );
    }

    #[tokio::test]
    async fn memory_recall_surfaces_kernel_error_as_502() {
        let kernel = Arc::new(RecordingKernelPort::new());
        kernel.push_recall_response(Err(ClientError::Forbidden("no".into())));
        let state = Cos72State {
            capabilities: Arc::new(vec![]),
            store: Cos72Store::open_memory().await.unwrap(),
            kernel: kernel.clone(),
        };
        let router = router(state);
        let (status, body) = post(&router, "/debug/memory-recall", json!({"query": "x"})).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        assert_eq!(body["error"]["code"], "kernel_error");
    }
}
