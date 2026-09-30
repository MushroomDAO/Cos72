//! Pure domain types — no IO, no sqlx, no axum, no SDK (docs/agent/
//! architecture.md「系统骨架」`core/`). `store` and `http` depend on this
//! module; this module depends on nothing else in the crate.

pub mod task;
