//! Background tasks that own the two things Cos72 does OUTSIDE an HTTP
//! request (docs/agent/architecture.md「系统骨架」`workers/`): polling
//! `approval_status` to credit/deny/expire awards ([`award_poller`], T1.3.1a)
//! and draining the memory outbox via `remember_once` ([`memory_pump`],
//! T1.3.1b).

pub mod award_poller;
pub mod memory_pump;
