//! Unified error body (docs/agent/spec.md「REST 路由」: `{"error": {"code":
//! "<snake_case>", "message": "..."}}`) + the `ValidatedJson` extractor that
//! routes every JSON-body rejection (missing field, wrong type, and —
//! because every T1.2.1 request struct is `#[serde(deny_unknown_fields)]` —
//! an unknown field too) through the same `invalid_request` shape instead of
//! axum's own plain-text `JsonRejection` body.

use axum::extract::{FromRequest, Request};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::de::DeserializeOwned;
use serde_json::json;

/// The T1.2.1-scope failure modes plus T1.3.1a's two advise-related ones
/// (docs/agent/spec.md「状态机」table + 「REST 路由」错误体 + 「submit 的精确步
/// 骤」第 1/3 步): `ApprovalUnavailable` (503 — `module.approval()` missing
/// or the proxied request carries no `request_id`/`approval_token`) and
/// `KernelError` (502 — any `advise` failure other than `ConnectionLost`/
/// `NotSent`, docs/agent/spec.md「submit 的精确步骤」第 3 步: "带闭集 kind").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    InvalidRequest(String),
    NotFound,
    InvalidTransition,
    NotClaimer,
    ApprovalUnavailable,
    KernelError(String),
    /// A store (sqlx) failure — the detail string is logged by the caller
    /// (`http::tasks::store_error`) via `tracing::error!`, never echoed back
    /// to the client (`message()` below returns a fixed, generic string).
    Internal,
}

impl ApiError {
    const fn status(&self) -> StatusCode {
        match self {
            Self::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::InvalidTransition => StatusCode::CONFLICT,
            Self::NotClaimer => StatusCode::FORBIDDEN,
            Self::ApprovalUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::KernelError(_) => StatusCode::BAD_GATEWAY,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    const fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "invalid_request",
            Self::NotFound => "not_found",
            Self::InvalidTransition => "invalid_transition",
            Self::NotClaimer => "not_claimer",
            Self::ApprovalUnavailable => "approval_unavailable",
            Self::KernelError(_) => "kernel_error",
            Self::Internal => "internal_error",
        }
    }

    fn message(&self) -> String {
        match self {
            Self::InvalidRequest(msg) => msg.clone(),
            Self::NotFound => "task not found".to_owned(),
            Self::InvalidTransition => "task is not in a state that allows this action".to_owned(),
            Self::NotClaimer => "only the member who claimed this task may submit it".to_owned(),
            Self::ApprovalUnavailable => {
                "the approval capability is unavailable for this request".to_owned()
            }
            Self::KernelError(msg) => msg.clone(),
            Self::Internal => "internal error".to_owned(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({"error": {"code": self.code(), "message": self.message()}});
        (self.status(), Json(body)).into_response()
    }
}

/// A `Json<T>` extractor whose rejection is [`ApiError::InvalidRequest`]
/// instead of axum's default plain-text `JsonRejection` body — every T1.2.1
/// handler uses this, never bare `Json<T>`, so `POST` bodies always fail with
/// the spec.md error shape (docs/agent/tasks.md T1.2.1 验收命令 #2
/// `unknown_body_fields_rejected`: an unknown field on a `deny_unknown_fields`
/// struct is a `JsonRejection` too, and lands here the same way).
pub struct ValidatedJson<T>(pub T);

impl<T, S> FromRequest<S> for ValidatedJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiError::InvalidRequest(rejection.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_and_code_pairs() {
        assert_eq!(
            ApiError::InvalidRequest("x".into()).status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(ApiError::NotFound.status(), StatusCode::NOT_FOUND);
        assert_eq!(ApiError::InvalidTransition.status(), StatusCode::CONFLICT);
        assert_eq!(ApiError::NotClaimer.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            ApiError::Internal.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            ApiError::InvalidRequest("x".into()).code(),
            "invalid_request"
        );
        assert_eq!(ApiError::NotFound.code(), "not_found");
        assert_eq!(ApiError::InvalidTransition.code(), "invalid_transition");
        assert_eq!(ApiError::NotClaimer.code(), "not_claimer");
        assert_eq!(ApiError::Internal.code(), "internal_error");
    }
}
