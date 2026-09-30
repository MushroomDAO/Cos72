//! Pure domain types — no IO, no sqlx, no axum, no SDK (docs/agent/
//! architecture.md「系统骨架」`core/`). `store` and `http` depend on this
//! module; this module depends on nothing else in the crate.

pub mod task;

/// RFC 3339 UTC, millisecond precision (docs/agent/spec.md「标识与校验」) —
/// the one place this format is produced, shared by `http::tasks`,
/// `store::ledger`'s callers, and `workers::award_poller`/`workers::
/// memory_pump` so no two call sites can silently drift apart on precision.
#[must_use]
pub fn now_rfc3339() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}
