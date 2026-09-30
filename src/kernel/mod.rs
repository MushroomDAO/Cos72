//! The ONLY module that knows the Agent24 SDK exists (docs/agent/
//! architecture.md「系统骨架」`kernel/`). `KernelPort` is Cos72's own narrow
//! trait (`emit` / `advise` / `approval_status` / `remember_once`) — `http`
//! and `store` (from T1.2.1 onward) only ever see this trait, never
//! `agent24_os_sdk` types directly, so a unit test can drive the whole stack
//! against [`RecordingKernelPort`] with no real kernel.
//!
//! T1.1.1 (docs/agent/tasks.md「开发范围」`src/kernel/`) implements `emit`
//! for real; `advise` / `approval_status` / `remember_once` return
//! [`ClientError::Unavailable`] placeholders here and are implemented for
//! real in T1.3.1 (advise + status polling + append-only ledger).

use agent24_os_sdk::{
    ApprovalAnswer, ApprovalSubmit, ClientError, EventSink, EventSinkConfig, Module, RememberOnce,
    UnavailableCause,
};
use serde_json::{Map, Value};

/// Cos72's own narrow view of the kernel — the production implementation
/// ([`SdkKernelPort`]) wraps `agent24_os_sdk`; tests drive
/// [`RecordingKernelPort`] instead. Plain `async fn` (stable since Rust
/// 1.75, well under the SDK's MSRV 1.88, docs/agent/architecture.md 核心
/// 判断 1) — nothing in this crate needs `Box<dyn KernelPort>`, so the trait
/// stays object-unsafe on purpose rather than paying for `async-trait`.
// Plain `async fn` in a trait is only object-unsafe and non-`Send`-bounded
// by default — irrelevant here since nothing in this crate ever boxes a
// `dyn KernelPort` (see the trait doc above) and every implementor this
// crate defines (`SdkKernelPort`, `RecordingKernelPort`) is `Send` anyway.
#[allow(async_fn_in_trait)]
pub trait KernelPort: Send + Sync + 'static {
    /// Fire-and-forget (docs/agent/architecture.md 核心判断 7: "事件走 SDK 的
    /// `EventSink`... 事件是通知不是真相") — never fails, never blocks the
    /// caller on the wire.
    fn emit(&self, kind: &str, payload: Map<String, Value>);

    /// # Errors
    /// See [`ClientError`]. T1.1.1: always
    /// `Unavailable { retryable: true, cause: NoProvider }` — no real advise
    /// wired yet (T1.3.1).
    async fn advise(&self, submit: &ApprovalSubmit<'_>) -> Result<ApprovalAnswer, ClientError>;

    /// # Errors
    /// See [`ClientError`]. T1.1.1: same placeholder as [`Self::advise`].
    async fn approval_status(&self, approval_id: &str) -> Result<ApprovalAnswer, ClientError>;

    /// # Errors
    /// See [`ClientError`]. T1.1.1: same placeholder as [`Self::advise`].
    async fn remember_once(
        &self,
        kind: &str,
        dedup_key: &str,
        body: Map<String, Value>,
    ) -> Result<RememberOnce, ClientError>;
}

/// T1.1.1 placeholder for the three capabilities this task does not wire up
/// yet — `retryable: true` because a caller in a LATER task (T1.3.1) that
/// actually calls this before its own real implementation lands should
/// retry, not treat "not implemented yet" as a permanent kernel refusal.
fn not_implemented_yet() -> ClientError {
    ClientError::Unavailable {
        retryable: true,
        cause: UnavailableCause::NoProvider,
    }
}

/// The production `KernelPort`: wraps the SDK's own `EventsClient` (via
/// `EventSink`, its bounded-queue fire-and-forget sink) for `emit`, and
/// placeholder-fails the other three (T1.1.1 scope).
pub struct SdkKernelPort {
    /// `None` when the kernel did not grant `events` at handshake (句柄可能
    /// 不在, docs/agent/architecture.md 不可动摇的边界 #6) — `emit` then
    /// degrades to a dropped, logged event rather than panicking.
    sink: Option<EventSink>,
}

impl SdkKernelPort {
    /// Wires whatever `module`'s `Offer` actually granted. Cos72's manifest
    /// only ever requests `events`, but the code is written as if the
    /// handle might not be there (架构 边界 #6), same posture Sin90's
    /// `wire_kernel_clients` uses.
    #[must_use]
    pub fn wire(module: &Module) -> Self {
        let sink = module
            .events()
            .map(|events| events.spawn_sink(EventSinkConfig::default()));
        if sink.is_none() {
            tracing::warn!(
                "cos72: kernel did not grant the events capability; every emit() will be \
                 dropped (logged instead)"
            );
        }
        Self { sink }
    }
}

impl KernelPort for SdkKernelPort {
    fn emit(&self, kind: &str, payload: Map<String, Value>) {
        match &self.sink {
            Some(sink) => sink.emit(kind, payload),
            None => tracing::warn!(kind, ?payload, "cos72: dropping event, no events sink"),
        }
    }

    async fn advise(&self, _submit: &ApprovalSubmit<'_>) -> Result<ApprovalAnswer, ClientError> {
        Err(not_implemented_yet())
    }

    async fn approval_status(&self, _approval_id: &str) -> Result<ApprovalAnswer, ClientError> {
        Err(not_implemented_yet())
    }

    async fn remember_once(
        &self,
        _kind: &str,
        _dedup_key: &str,
        _body: Map<String, Value>,
    ) -> Result<RememberOnce, ClientError> {
        Err(not_implemented_yet())
    }
}

/// A recording, in-memory `KernelPort` for unit tests (docs/agent/tasks.md
/// T1.1.1「开发范围」"测试用记录型假实现") — records every `emit` call so a
/// test can assert exactly which events a code path produced, without a
/// real kernel or the SDK's own `testing::fake_kernel` wire fixture. Not
/// used by any T1.1.1 test itself (no business route calls `emit` yet); it
/// exists now so T1.2.1's route/event tests (docs/agent/tasks.md T1.2.1
/// 验收命令 #3) can depend on it immediately.
#[cfg(any(test, feature = "test-hooks"))]
#[derive(Default)]
pub struct RecordingKernelPort {
    emitted: std::sync::Mutex<Vec<(String, Map<String, Value>)>>,
}

#[cfg(any(test, feature = "test-hooks"))]
impl RecordingKernelPort {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every `(kind, payload)` pair `emit` was called with, in call order.
    #[must_use]
    pub fn emitted(&self) -> Vec<(String, Map<String, Value>)> {
        self.emitted.lock().expect("emitted mutex poisoned").clone()
    }
}

#[cfg(any(test, feature = "test-hooks"))]
impl KernelPort for RecordingKernelPort {
    fn emit(&self, kind: &str, payload: Map<String, Value>) {
        self.emitted
            .lock()
            .expect("emitted mutex poisoned")
            .push((kind.to_owned(), payload));
    }

    async fn advise(&self, _submit: &ApprovalSubmit<'_>) -> Result<ApprovalAnswer, ClientError> {
        Err(not_implemented_yet())
    }

    async fn approval_status(&self, _approval_id: &str) -> Result<ApprovalAnswer, ClientError> {
        Err(not_implemented_yet())
    }

    async fn remember_once(
        &self,
        _kind: &str,
        _dedup_key: &str,
        _body: Map<String, Value>,
    ) -> Result<RememberOnce, ClientError> {
        Err(not_implemented_yet())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent24_os_sdk::testing::{fake_kernel, noop_hook, read_request, FakeEndpoint};
    use agent24_os_sdk::{EventsClient, Module};
    use serde_json::json;

    fn tempdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cos72-kernel-test-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// docs/agent/tasks.md T1.1.1 验收命令 #3, first case: the handshake
    /// this crate's own `main.rs` will run sends EXACTLY the manifest's
    /// `kernel_capabilities` (not a hand-maintained second list) and the
    /// SDK's declared protocol range `{min:1,max:1}`.
    ///
    /// 正对照 (docs/agent/tasks.md T1.1.1 验收命令 #1): if `domain-os.yml`'s
    /// `kernel_capabilities` grows a fourth entry (e.g. `scheduler`), this
    /// assertion goes red — proven by hand: temporarily add `scheduler` to
    /// the fixture list below and re-run; it fails.
    #[tokio::test]
    async fn handshake_declares_exactly_manifest_capabilities() {
        let dir = tempdir();
        let (env, endpoint) = FakeEndpoint::bind(&dir);
        let connect = tokio::spawn(
            Module::builder(crate::MANIFEST)
                .with_env(env, noop_hook())
                .connect(),
        );
        let (params, _peer) = endpoint
            .accept_initialize(json!({"protocol_version": 1, "offer": {"provides": []}}))
            .await;
        let module = connect.await.expect("connect task panicked");
        module.expect("handshake must succeed against the fake endpoint");

        assert_eq!(
            params["capabilities"],
            json!(["events", "memory", "approval"]),
            "must send exactly the manifest's kernel_capabilities, not a hand-maintained \
             second list: {params}"
        );
        assert_eq!(
            params["protocol_versions"],
            json!({"min": 1, "max": 1}),
            "must declare the SDK's own protocol range: {params}"
        );
    }

    /// docs/agent/tasks.md T1.1.1 验收命令 #3, second case: `emit` really
    /// puts `_a24/events/emit` on the wire with the given `kind`/`payload`.
    #[tokio::test]
    async fn emit_sends_events_emit_with_kind_and_payload() {
        let (conn, mut peer) = fake_kernel(vec!["_a24/events/".to_string()]).await;
        let events = EventsClient::new(&conn).expect("events must be Some: offer granted it");
        let sink = events.spawn_sink(EventSinkConfig::default());
        let port = SdkKernelPort { sink: Some(sink) };

        let mut payload = Map::new();
        payload.insert("task_id".to_owned(), json!("tsk_01"));
        port.emit("task.published", payload.clone());

        let req = read_request(&mut peer).await;
        assert_eq!(req["method"], "_a24/events/emit", "{req}");
        assert_eq!(req["params"]["kind"], "task.published", "{req}");
        assert_eq!(req["params"]["payload"], Value::Object(payload), "{req}");
    }

    /// Positive control for [`RecordingKernelPort`] itself: what it records
    /// is exactly what was emitted, in order.
    #[test]
    fn recording_kernel_port_records_emits_in_order() {
        let port = RecordingKernelPort::new();
        port.emit("a", Map::new());
        let mut p = Map::new();
        p.insert("x".to_owned(), json!(1));
        port.emit("b", p.clone());
        assert_eq!(
            port.emitted(),
            vec![("a".to_owned(), Map::new()), ("b".to_owned(), p)]
        );
    }
}
