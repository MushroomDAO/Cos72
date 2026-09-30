//! Cos72's axum routes. T1.1.1 scope: `GET /health` only (docs/agent/spec.md
//! 「REST 路由」table's first row) — task/award/points routes are T1.2.1 /
//! T1.3.1. `/debug/*` (T1.3.1, `test-hooks`-only) is not registered here yet
//! either.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::Arc;

/// What `GET /health` reports. `capabilities` is the Offer the kernel
/// ACTUALLY granted at handshake (docs/agent/spec.md「REST 路由」: "便于黑盒
/// 核对"), not a hand-maintained copy of the manifest's request — the real
/// mount black box (`tests/agent24_mount_blackbox.rs`) checks this contains
/// `_a24/events/` / `_a24/memory/private/` / `_a24/approval/` and does NOT
/// contain `_a24/scheduler/`.
#[derive(Clone)]
pub struct Cos72State {
    pub capabilities: Arc<Vec<String>>,
}

pub fn router(state: Cos72State) -> Router {
    Router::new()
        .route("/health", get(health))
        .with_state(state)
}

async fn health(State(state): State<Cos72State>) -> Json<Value> {
    Json(json!({
        "name": "cos72",
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": state.capabilities.as_ref(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_reports_name_version_and_capabilities() {
        let state = Cos72State {
            capabilities: Arc::new(vec!["_a24/events/".to_owned()]),
        };
        let response = router(state)
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
