//! The ONLY module that knows the Agent24 SDK exists (docs/agent/
//! architecture.md「系统骨架」`kernel/`). `KernelPort` is Cos72's own narrow
//! trait (`emit` / `approval_available` / `advise` / `approval_status` /
//! `remember_once`) — `http`, `store`, and `workers` only ever see this
//! trait, never `agent24_os_sdk` types directly, so a unit test can drive
//! the whole stack against [`RecordingKernelPort`] with no real kernel.
//!
//! T1.1.1 implemented `emit` for real; T1.3.1a (ME4-5.3.3b, money piece)
//! implemented `approval_available` / `advise` / `approval_status` for real,
//! wired to `module.approval()`. T1.3.1b (this piece, ME4-5.3.3b, memory
//! piece — jason's 2026-09-30 split) implements `memory_available` /
//! `remember_once` / `recall` for real, wired to `module.memory()`
//! (docs/agent/architecture.md 核心判断 6: "记忆写入只经 Cos72 自己的 outbox，由
//! 单一泵串行调 remember_once"). `recall` exists only so the `test-hooks`
//! `/debug/memory-recall` route (spec.md「REST 路由」) can query the kernel's
//! private memory directly for black-box verification.

use std::future::Future;

use agent24_os_sdk::{
    ApprovalAnswer, ApprovalClient, ApprovalSubmit, ClientError, EventSink, EventSinkConfig,
    MemoryClient, Module, RecallPage, RememberOnce, UnavailableCause,
};
use serde_json::{Map, Value};

/// Cos72's own narrow view of the kernel — the production implementation
/// ([`SdkKernelPort`]) wraps `agent24_os_sdk`; tests drive
/// [`RecordingKernelPort`] instead. The three async methods are written as
/// plain `fn`s returning `impl Future<..> + Send` (RPITIT, stable well under
/// the SDK's MSRV 1.88) rather than `async fn` — `workers::award_poller::
/// spawn_loop` hands the resulting futures to `tokio::spawn`, which requires
/// `Send`, and a bare `async fn` in a trait does not carry that bound by
/// default. Nothing in this crate needs `Box<dyn KernelPort>`, so the trait
/// stays object-unsafe on purpose rather than paying for `async-trait`.
pub trait KernelPort: Send + Sync + 'static {
    /// Fire-and-forget (docs/agent/architecture.md 核心判断 7: "事件走 SDK 的
    /// `EventSink`... 事件是通知不是真相") — never fails, never blocks the
    /// caller on the wire.
    fn emit(&self, kind: &str, payload: Map<String, Value>);

    /// Whether this generation's handshake actually granted `approval`
    /// (docs/agent/spec.md「submit 的精确步骤」第 1 步: "module.approval() 为
    /// None … → 503 approval_unavailable，不写库") — checked by the submit
    /// handler BEFORE any store write, never inferred from an `advise`
    /// call's own error.
    fn approval_available(&self) -> bool;

    /// # Errors
    /// See [`ClientError`]. docs/agent/architecture.md 不可动摇的边界 #3:
    /// only ever called synchronously inside the submit HTTP handler, with
    /// the CURRENT request's `request_id`/`approval_token`.
    fn advise(
        &self,
        submit: &ApprovalSubmit<'_>,
    ) -> impl Future<Output = Result<ApprovalAnswer, ClientError>> + Send;

    /// # Errors
    /// See [`ClientError`].
    fn approval_status(
        &self,
        approval_id: &str,
    ) -> impl Future<Output = Result<ApprovalAnswer, ClientError>> + Send;

    /// Whether this generation's handshake actually granted `memory`
    /// (docs/agent/spec.md「记忆泵」: "没有 memory 能力（`module.memory()` 为
    /// `None`）→ 泵不启动，outbox 行保持 `pending`，不报错") — checked once at
    /// startup (`main.rs`, before spawning `workers::memory_pump`), not per
    /// call.
    fn memory_available(&self) -> bool;

    /// # Errors
    /// See [`ClientError`]. docs/agent/architecture.md 不可动摇的边界 #4:
    /// only ever called from `workers::memory_pump` — never the HTTP handler
    /// or the award poller (structurally checked, `tests/structure.rs`).
    fn remember_once(
        &self,
        kind: &str,
        dedup_key: &str,
        body: Map<String, Value>,
    ) -> impl Future<Output = Result<RememberOnce, ClientError>> + Send;

    /// # Errors
    /// See [`ClientError`]. Only used by the `test-hooks`-only
    /// `/debug/memory-recall` route (spec.md「REST 路由」: "黑盒核对记忆与隔离
    /// 用").
    fn recall(
        &self,
        query: &str,
        page_size: usize,
    ) -> impl Future<Output = Result<RecallPage, ClientError>> + Send;
}

/// The kernel genuinely did not grant `approval` at handshake — a real,
/// permanent fact about the current generation's `Offer`. `retryable: false`
/// — a fresh advise/status call in the SAME generation will get the same
/// answer; only a new generation (new handshake) could change it, and the
/// submit handler never reaches this anyway (it checks
/// [`KernelPort::approval_available`] first and returns 503 without calling
/// `advise` at all — this only guards [`SdkKernelPort::approval_status`]
/// being called by a worker after capabilities were somehow lost, which does
/// not happen mid-generation but is written defensively rather than
/// `unreachable!()`).
fn no_approval_capability() -> ClientError {
    ClientError::Unavailable {
        retryable: false,
        cause: UnavailableCause::NoProvider,
    }
}

/// Same posture as [`no_approval_capability`], for `memory`. `main.rs` never
/// spawns `workers::memory_pump` when [`KernelPort::memory_available`] is
/// `false`, so this only guards a call that should not happen in practice.
fn no_memory_capability() -> ClientError {
    ClientError::Unavailable {
        retryable: false,
        cause: UnavailableCause::NoProvider,
    }
}

/// The production `KernelPort`: wraps the SDK's own `EventsClient` (via
/// `EventSink`, its bounded-queue fire-and-forget sink) for `emit`, the
/// SDK's `ApprovalClient` for `advise`/`approval_status` (T1.3.1a), and the
/// SDK's `MemoryClient` for `remember_once`/`recall` (T1.3.1b).
pub struct SdkKernelPort {
    /// `None` when the kernel did not grant `events` at handshake (句柄可能
    /// 不在, docs/agent/architecture.md 不可动摇的边界 #6) — `emit` then
    /// degrades to a dropped, logged event rather than panicking.
    sink: Option<EventSink>,
    /// `None` when the kernel did not grant `approval` at handshake — same
    /// "handle may not be there" posture.
    approval: Option<ApprovalClient>,
    /// `None` when the kernel did not grant `memory` at handshake — same
    /// posture.
    memory: Option<MemoryClient>,
}

impl SdkKernelPort {
    /// Wires whatever `module`'s `Offer` actually granted. Cos72's manifest
    /// requests `[events, memory, approval]`, but the code is written as if
    /// any one handle might not be there (架构 边界 #6), same posture Sin90's
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
        let approval = module.approval();
        if approval.is_none() {
            tracing::warn!(
                "cos72: kernel did not grant the approval capability; submit will answer 503 \
                 approval_unavailable"
            );
        }
        let memory = module.memory();
        if memory.is_none() {
            tracing::warn!(
                "cos72: kernel did not grant the memory capability; the memory pump will not \
                 start, outbox rows stay pending"
            );
        }
        Self {
            sink,
            approval,
            memory,
        }
    }
}

impl KernelPort for SdkKernelPort {
    fn emit(&self, kind: &str, payload: Map<String, Value>) {
        match &self.sink {
            Some(sink) => sink.emit(kind, payload),
            None => tracing::warn!(kind, ?payload, "cos72: dropping event, no events sink"),
        }
    }

    fn approval_available(&self) -> bool {
        self.approval.is_some()
    }

    async fn advise(&self, submit: &ApprovalSubmit<'_>) -> Result<ApprovalAnswer, ClientError> {
        match &self.approval {
            Some(client) => client.advise(submit).await,
            None => Err(no_approval_capability()),
        }
    }

    async fn approval_status(&self, approval_id: &str) -> Result<ApprovalAnswer, ClientError> {
        match &self.approval {
            Some(client) => client.status(approval_id).await,
            None => Err(no_approval_capability()),
        }
    }

    fn memory_available(&self) -> bool {
        self.memory.is_some()
    }

    async fn remember_once(
        &self,
        kind: &str,
        dedup_key: &str,
        body: Map<String, Value>,
    ) -> Result<RememberOnce, ClientError> {
        match &self.memory {
            Some(client) => client.remember_once(kind, dedup_key, body, None).await,
            None => Err(no_memory_capability()),
        }
    }

    async fn recall(&self, query: &str, page_size: usize) -> Result<RecallPage, ClientError> {
        match &self.memory {
            Some(client) => client.recall(query, page_size, None, None).await,
            None => Err(no_memory_capability()),
        }
    }
}

/// A recording, in-memory `KernelPort` for unit tests (docs/agent/tasks.md
/// T1.1.1「开发范围」"测试用记录型假实现") — records every `emit`/`advise` call
/// so a test can assert exactly which events/approvals a code path
/// produced, and lets a test script canned `advise`/`approval_status`
/// answers, without a real kernel or the SDK's own `testing::fake_kernel`
/// wire fixture (that fixture is reserved for the handful of tests that
/// pin down the WIRE shape itself, e.g. [`SdkKernelPort`]'s own tests
/// below).
#[cfg(any(test, feature = "test-hooks"))]
pub struct RecordingKernelPort {
    emitted: std::sync::Mutex<Vec<(String, Map<String, Value>)>>,
    approval_available: std::sync::atomic::AtomicBool,
    advise_calls: std::sync::Mutex<Vec<AdviseCall>>,
    advise_responses:
        std::sync::Mutex<std::collections::VecDeque<Result<ApprovalAnswer, ClientError>>>,
    status_calls: std::sync::Mutex<Vec<String>>,
    status_answers:
        std::sync::Mutex<std::collections::HashMap<String, Result<ApprovalAnswer, ClientError>>>,
    memory_available: std::sync::atomic::AtomicBool,
    remember_calls: std::sync::Mutex<Vec<RememberCall>>,
    remember_responses:
        std::sync::Mutex<std::collections::VecDeque<Result<RememberOnce, ClientError>>>,
    recall_calls: std::sync::Mutex<Vec<(String, usize)>>,
    recall_responses: std::sync::Mutex<std::collections::VecDeque<Result<RecallPage, ClientError>>>,
}

/// One recorded `remember_once` call (docs/agent/tasks.md T1.3.1 验收命令
/// #4 `pump_calls_remember_once_with_namespaced_dedup_key`).
#[cfg(any(test, feature = "test-hooks"))]
#[derive(Debug, Clone, PartialEq)]
pub struct RememberCall {
    pub kind: String,
    pub dedup_key: String,
    pub body: Map<String, Value>,
}

/// One recorded `advise` call — everything a T1.3.1a test needs to assert
/// about the request that produced it (docs/agent/tasks.md T1.3.1 验收命
/// 令 #1 `submit_advises_inside_request_with_its_request_id_and_token`).
#[cfg(any(test, feature = "test-hooks"))]
#[derive(Debug, Clone, PartialEq)]
pub struct AdviseCall {
    pub action: String,
    pub target: Option<String>,
    pub payload: Value,
    pub request_id: String,
    /// Kept as the SDK's own [`agent24_os_sdk::ApprovalToken`] rather than a
    /// `String` — that type's `as_str` is `pub(crate)` to the SDK (the token
    /// is a secret, deliberately unreadable outside it), so a test compares
    /// two calls' tokens via `PartialEq` (or against one built the same way
    /// via `RequestContext::from_headers`) instead of by printing the value.
    pub approval_token: agent24_os_sdk::ApprovalToken,
}

#[cfg(any(test, feature = "test-hooks"))]
impl Default for RecordingKernelPort {
    /// `approval_available` defaults to `true` — most T1.3.1a tests exercise
    /// the "approval capability present" path; the one test that needs the
    /// opposite calls [`RecordingKernelPort::set_approval_available`] first.
    fn default() -> Self {
        Self {
            emitted: std::sync::Mutex::new(Vec::new()),
            approval_available: std::sync::atomic::AtomicBool::new(true),
            advise_calls: std::sync::Mutex::new(Vec::new()),
            advise_responses: std::sync::Mutex::new(std::collections::VecDeque::new()),
            status_calls: std::sync::Mutex::new(Vec::new()),
            status_answers: std::sync::Mutex::new(std::collections::HashMap::new()),
            memory_available: std::sync::atomic::AtomicBool::new(true),
            remember_calls: std::sync::Mutex::new(Vec::new()),
            remember_responses: std::sync::Mutex::new(std::collections::VecDeque::new()),
            recall_calls: std::sync::Mutex::new(Vec::new()),
            recall_responses: std::sync::Mutex::new(std::collections::VecDeque::new()),
        }
    }
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

    pub fn set_approval_available(&self, available: bool) {
        self.approval_available
            .store(available, std::sync::atomic::Ordering::SeqCst);
    }

    /// Queue the next `advise` call's answer (FIFO) — pushed answers are
    /// popped in the order `advise` is called, letting a test script e.g.
    /// "timeout, then success" for the same submit.
    pub fn push_advise_response(&self, response: Result<ApprovalAnswer, ClientError>) {
        self.advise_responses
            .lock()
            .expect("advise_responses mutex poisoned")
            .push_back(response);
    }

    /// Every `advise` call, in call order.
    #[must_use]
    pub fn advise_calls(&self) -> Vec<AdviseCall> {
        self.advise_calls
            .lock()
            .expect("advise_calls mutex poisoned")
            .clone()
    }

    /// Set (or replace) what `approval_status(approval_id)` answers from now
    /// on — a test updates this between poller ticks to script an
    /// approval's lifecycle (`pending` → `approved`, etc.).
    pub fn set_status(&self, approval_id: &str, answer: Result<ApprovalAnswer, ClientError>) {
        self.status_answers
            .lock()
            .expect("status_answers mutex poisoned")
            .insert(approval_id.to_owned(), answer);
    }

    /// Every `approval_id` `approval_status` was called with, in call order
    /// (including repeats).
    #[must_use]
    pub fn status_calls(&self) -> Vec<String> {
        self.status_calls
            .lock()
            .expect("status_calls mutex poisoned")
            .clone()
    }

    pub fn set_memory_available(&self, available: bool) {
        self.memory_available
            .store(available, std::sync::atomic::Ordering::SeqCst);
    }

    /// Queue the next `remember_once` call's answer (FIFO).
    pub fn push_remember_response(&self, response: Result<RememberOnce, ClientError>) {
        self.remember_responses
            .lock()
            .expect("remember_responses mutex poisoned")
            .push_back(response);
    }

    /// Every `remember_once` call, in call order.
    #[must_use]
    pub fn remember_calls(&self) -> Vec<RememberCall> {
        self.remember_calls
            .lock()
            .expect("remember_calls mutex poisoned")
            .clone()
    }

    /// Queue the next `recall` call's answer (FIFO) — the `test-hooks`
    /// `/debug/memory-recall` route's own tests use this.
    pub fn push_recall_response(&self, response: Result<RecallPage, ClientError>) {
        self.recall_responses
            .lock()
            .expect("recall_responses mutex poisoned")
            .push_back(response);
    }

    /// Every `(query, page_size)` pair `recall` was called with, in order.
    #[must_use]
    pub fn recall_calls(&self) -> Vec<(String, usize)> {
        self.recall_calls
            .lock()
            .expect("recall_calls mutex poisoned")
            .clone()
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

    fn approval_available(&self) -> bool {
        self.approval_available
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn advise(&self, submit: &ApprovalSubmit<'_>) -> Result<ApprovalAnswer, ClientError> {
        self.advise_calls
            .lock()
            .expect("advise_calls mutex poisoned")
            .push(AdviseCall {
                action: submit.action.to_owned(),
                target: submit.target.map(str::to_owned),
                payload: submit.payload.clone(),
                request_id: submit.request_id.as_str().to_owned(),
                approval_token: submit.approval_token.clone(),
            });
        self.advise_responses
            .lock()
            .expect("advise_responses mutex poisoned")
            .pop_front()
            .unwrap_or_else(|| {
                panic!("RecordingKernelPort::advise called with no queued response left")
            })
    }

    async fn approval_status(&self, approval_id: &str) -> Result<ApprovalAnswer, ClientError> {
        self.status_calls
            .lock()
            .expect("status_calls mutex poisoned")
            .push(approval_id.to_owned());
        self.status_answers
            .lock()
            .expect("status_answers mutex poisoned")
            .get(approval_id)
            .cloned()
            .unwrap_or_else(|| Err(ClientError::NotFound(approval_id.to_owned())))
    }

    fn memory_available(&self) -> bool {
        self.memory_available
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn remember_once(
        &self,
        kind: &str,
        dedup_key: &str,
        body: Map<String, Value>,
    ) -> Result<RememberOnce, ClientError> {
        self.remember_calls
            .lock()
            .expect("remember_calls mutex poisoned")
            .push(RememberCall {
                kind: kind.to_owned(),
                dedup_key: dedup_key.to_owned(),
                body,
            });
        self.remember_responses
            .lock()
            .expect("remember_responses mutex poisoned")
            .pop_front()
            .unwrap_or_else(|| {
                panic!("RecordingKernelPort::remember_once called with no queued response left")
            })
    }

    async fn recall(&self, query: &str, page_size: usize) -> Result<RecallPage, ClientError> {
        self.recall_calls
            .lock()
            .expect("recall_calls mutex poisoned")
            .push((query.to_owned(), page_size));
        self.recall_responses
            .lock()
            .expect("recall_responses mutex poisoned")
            .pop_front()
            .unwrap_or_else(|| {
                panic!("RecordingKernelPort::recall called with no queued response left")
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent24_os_sdk::testing::{fake_kernel, noop_hook, read_request, respond, FakeEndpoint};
    use agent24_os_sdk::{ApprovalDecision, ApprovalKind, EventsClient, Module, RequestContext};
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
        let port = SdkKernelPort {
            sink: Some(sink),
            approval: None,
            memory: None,
        };

        let mut payload = Map::new();
        payload.insert("task_id".to_owned(), json!("tsk_01"));
        port.emit("task.published", payload.clone());

        let req = read_request(&mut peer).await;
        assert_eq!(req["method"], "_a24/events/emit", "{req}");
        assert_eq!(req["params"]["kind"], "task.published", "{req}");
        assert_eq!(req["params"]["payload"], Value::Object(payload), "{req}");
    }

    /// docs/agent/tasks.md T1.3.1 验收命令 #1 (wire pin): `SdkKernelPort::
    /// advise` puts `_a24/approval/advise` on the wire carrying the current
    /// request's `request_id`/`approval_token` and the given payload.
    #[tokio::test]
    async fn advise_sends_approval_advise_with_request_id_and_token() {
        let (conn, mut peer) = fake_kernel(vec!["_a24/approval/".to_string()]).await;
        let approval = ApprovalClient::new(&conn).expect("offer covers approval");
        let port = SdkKernelPort {
            sink: None,
            approval: Some(approval),
            memory: None,
        };
        assert!(port.approval_available());

        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            agent24_os_proto_header_request_id(),
            "req-1".parse().unwrap(),
        );
        headers.insert(
            agent24_os_proto_header_approval_token(),
            "secret-1".parse().unwrap(),
        );
        let ctx = RequestContext::from_headers(&headers);
        let submit = ApprovalSubmit {
            action: "cos72.award_points",
            target: Some("tsk_1"),
            payload: json!({"award_id": "awd_1"}),
            request_id: ctx.request_id.as_ref().unwrap(),
            approval_token: ctx.approval_token.as_ref().unwrap(),
        };

        let (answer, ()) = tokio::join!(port.advise(&submit), async {
            let req = read_request(&mut peer).await;
            assert_eq!(req["method"], "_a24/approval/advise", "{req}");
            assert_eq!(req["params"]["request_id"], "req-1", "{req}");
            assert_eq!(req["params"]["approval_token"], "secret-1", "{req}");
            assert_eq!(req["params"]["payload"]["award_id"], "awd_1", "{req}");
            respond(
                &mut peer,
                &req,
                json!({
                    "approval_id": "appr-1",
                    "kind": "advise",
                    "binding": false,
                    "decision": "pending",
                    "executed_at": null,
                }),
            )
            .await;
        });
        let answer = answer.unwrap();
        assert_eq!(answer.approval_id, "appr-1");
        assert_eq!(answer.kind, ApprovalKind::Advise);
        assert_eq!(answer.decision, ApprovalDecision::Pending);
    }

    /// docs/agent/tasks.md T1.3.1 验收命令 #1 (wire pin, negative control):
    /// no `approval` capability granted → `advise`/`approval_status` fail
    /// locally WITHOUT putting anything on the wire.
    #[tokio::test]
    async fn no_approval_capability_fails_without_touching_the_wire() {
        let port = SdkKernelPort {
            sink: None,
            approval: None,
            memory: None,
        };
        assert!(!port.approval_available());
        let err = port
            .approval_status("appr-x")
            .await
            .expect_err("must fail with no approval client");
        assert!(matches!(
            err,
            ClientError::Unavailable {
                retryable: false,
                ..
            }
        ));
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

    /// Positive control: `RecordingKernelPort::advise` records the call and
    /// returns queued responses in FIFO order.
    #[tokio::test]
    async fn recording_kernel_port_advise_records_and_replays_in_order() {
        let port = RecordingKernelPort::new();
        port.push_advise_response(Err(ClientError::Timeout("t".into())));
        port.push_advise_response(Ok(ApprovalAnswer {
            approval_id: "appr-1".into(),
            kind: ApprovalKind::Advise,
            binding: false,
            decision: ApprovalDecision::Pending,
            executed_at: None,
        }));

        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            agent24_os_proto_header_request_id(),
            "req-1".parse().unwrap(),
        );
        headers.insert(
            agent24_os_proto_header_approval_token(),
            "tok-1".parse().unwrap(),
        );
        let ctx = RequestContext::from_headers(&headers);
        let submit = ApprovalSubmit {
            action: "cos72.award_points",
            target: None,
            payload: json!({}),
            request_id: ctx.request_id.as_ref().unwrap(),
            approval_token: ctx.approval_token.as_ref().unwrap(),
        };

        let first = port.advise(&submit).await;
        assert!(matches!(first, Err(ClientError::Timeout(_))));
        let second = port.advise(&submit).await.unwrap();
        assert_eq!(second.approval_id, "appr-1");
        assert_eq!(port.advise_calls().len(), 2);
        assert_eq!(port.advise_calls()[0].request_id, "req-1");
        assert_eq!(port.advise_calls()[1].request_id, "req-1");
    }

    /// Test-only helpers so this module's own tests can build a
    /// [`RequestContext`] without depending on `agent24_os_proto`'s header
    /// name constants directly (kept private to this test module).
    fn agent24_os_proto_header_request_id() -> &'static str {
        "x-a24-request-id"
    }
    fn agent24_os_proto_header_approval_token() -> &'static str {
        "x-a24-approval-token"
    }
}
