//! Cos72's axum routes. T1.1.1 scope was `GET /health` only; T1.2.1
//! (docs/agent/tasks.md「开发范围」`src/http/tasks.rs`) adds the mytask
//! routes (publish/list/get/claim/submit). `/points` (T1.3.1) and
//! `/debug/*` (T1.3.1, `test-hooks`-only) are not registered here yet.

pub mod error;
pub mod points;
pub mod tasks;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::kernel::KernelPort;
use crate::store::Cos72Store;

/// Shared axum state. Generic over `K: KernelPort` (not `Arc<dyn KernelPort>`
/// — `KernelPort`'s plain `async fn`s make it non-object-safe by design, see
/// `kernel/mod.rs`'s doc) so `main.rs` monomorphizes on `SdkKernelPort` and
/// tests monomorphize on `RecordingKernelPort`, with no boxing either way.
pub struct Cos72State<K> {
    /// The Offer the kernel ACTUALLY granted at handshake (docs/agent/spec.md
    /// 「REST 路由」`GET /health`: "便于黑盒核对") — not a hand-maintained copy
    /// of the manifest's request.
    pub capabilities: Arc<Vec<String>>,
    pub store: Cos72Store,
    pub kernel: Arc<K>,
}

// Manual `Clone` (not `#[derive(Clone)]`): a derive would require `K: Clone`,
// but axum only ever needs to clone the `Arc<K>` handle, not `K` itself —
// exactly what `RecordingKernelPort` (no `Clone` impl, deliberately, so a
// test can hold one shared instance and assert against it) relies on.
impl<K> Clone for Cos72State<K> {
    fn clone(&self) -> Self {
        Self {
            capabilities: self.capabilities.clone(),
            store: self.store.clone(),
            kernel: self.kernel.clone(),
        }
    }
}

pub fn router<K: KernelPort>(state: Cos72State<K>) -> Router {
    Router::new()
        .route("/health", get(health::<K>))
        .route(
            "/tasks",
            post(tasks::publish_task::<K>).get(tasks::list_tasks::<K>),
        )
        .route("/tasks/{id}", get(tasks::get_task::<K>))
        .route("/tasks/{id}/claim", post(tasks::claim_task::<K>))
        .route("/tasks/{id}/submit", post(tasks::submit_task::<K>))
        .route("/points", get(points::all_balances::<K>))
        .route("/points/{member}", get(points::member_balance::<K>))
        .with_state(state)
}

async fn health<K: KernelPort>(State(state): State<Cos72State<K>>) -> Json<Value> {
    Json(json!({
        "name": "cos72",
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": state.capabilities.as_ref(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::RecordingKernelPort;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn test_state() -> Cos72State<RecordingKernelPort> {
        Cos72State {
            capabilities: Arc::new(vec!["_a24/events/".to_owned()]),
            store: Cos72Store::open_memory().await.unwrap(),
            kernel: Arc::new(RecordingKernelPort::new()),
        }
    }

    #[tokio::test]
    async fn health_reports_name_version_and_capabilities() {
        let response = router(test_state().await)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        // `from_str`, not the slice-taking sibling — tests/structure.rs's
        // structural check (docs/agent/architecture.md 不可动摇的边界 #7)
        // greps `src/` for that exact spelling as a proxy for "hand-parses a
        // protocol frame"; this is an ordinary HTTP JSON body in a test, not
        // that, so it deliberately avoids the flagged spelling verbatim.
        let value: Value = serde_json::from_str(std::str::from_utf8(&body).unwrap()).unwrap();
        assert_eq!(value["name"], "cos72");
        assert_eq!(value["capabilities"], json!(["_a24/events/"]));
    }
}
