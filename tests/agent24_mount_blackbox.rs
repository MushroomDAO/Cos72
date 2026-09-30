//! Real-`agent24d` mount verification (docs/agent/tasks.md T1.1.1 验收命令
//! #5) — a real, already-built `agent24d` binary mounts a real, already-
//! built `cos72` binary as an out-of-process package, and the health route
//! round-trips through the real constrained proxy, ending in a real
//! `module.ready` event observed at the real WS boundary.
//!
//! Requires a sibling Agent24 checkout (`AGENT24_CHECKOUT` env var, default
//! `../Agent24`) with a workspace at `<checkout>/rust`. `#[ignore]`d so
//! `cargo test` stays green with no such checkout present (CI, a clone with
//! only this repo); run explicitly:
//!
//!   AGENT24_CHECKOUT=$HOME/Dev/auraai/Agent24 \
//!     cargo test --features test-hooks --test agent24_mount_blackbox -- --ignored --test-threads=1
//!
//! `--features test-hooks` is required from `cos72_award_roundtrip_under_real_daemon`
//! onward (T1.3.1b) — every test in this file that calls the `test-hooks`-only
//! `POST /debug/memory-recall` route (`src/http/debug.rs`) needs the real
//! `cos72` binary under test (`CARGO_BIN_EXE_cos72`, built once for the whole
//! `cargo test` invocation, shared by every test in this file) built with
//! that feature on. Omitting it does not fail fast — it 404s the debug route,
//! which then reads as a `wait_for_memory_recall` timeout with a confusing
//! daemon log.
//!
//! T1.4.1 additionally needs a sibling Sin90 checkout ALREADY MIGRATED to
//! `agent24-os-sdk` (`SIN90_CHECKOUT` env var — no default guess, unlike
//! `AGENT24_CHECKOUT`: docs/agent/tasks.md T1.4.1 验收命令 #3 requires this
//! to be a hard panic, not a silent skip, when the checkout is missing) with
//! a `Cargo.toml` at its root and its own `test-hooks` feature (for its
//! `POST /debug/kernel-roundtrip` route, `src/adapter_agent24/kernel_roundtrip.rs`
//! in that repo). Full run, both checkouts:
//!
//!   AGENT24_CHECKOUT=$HOME/Dev/auraai/Agent24 \
//!     SIN90_CHECKOUT=$HOME/Dev/auraai/sin90-design \
//!     cargo test --features test-hooks --test agent24_mount_blackbox -- --ignored --test-threads=1
//!
//! Harness shape (install/start/WS-subscribe/http helpers) mirrors Sin90's
//! own `tests/agent24_mount_blackbox.rs` (docs/agent/acceptance.md「待补能力
//! 清单」: "真实挂载黑盒夹具（装包/起 daemon/WS 订阅）| 高 | Sin90
//! tests/agent24_mount_blackbox.rs") — a much smaller slice of it, scoped to
//! T1.1.1's one acceptance case (mount + health + module.ready + the 401
//! negative control); T1.2.1/T1.3.1/T1.4.1 grow this file with their own
//! cases the same way Sin90's grew across its own M0–M5. T1.4.1's own two
//! cases (`cos72_full_flow_real_mount`, `cos72_and_sin90_coexist_isolated`)
//! additionally build and install a REAL `sin90` binary from `SIN90_CHECKOUT`
//! next to Cos72's — mirroring, at a source level (this crate cannot depend
//! on a sibling repo's crate), Sin90's own `build_sin90_with_test_hooks` /
//! `install_sin90`.
#![cfg(unix)]

use std::io::{BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Locate the Agent24 checkout. `../Agent24` is this environment's actual
/// layout (both repos are sibling directories under the same parent) — an
/// env var override exists for anyone whose layout differs.
fn agent24_checkout() -> Option<PathBuf> {
    let dir = std::env::var("AGENT24_CHECKOUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("Agent24")
        });
    dir.join("rust/Cargo.toml").is_file().then_some(dir)
}

/// Build `agent24d` in the Agent24 checkout and return the binary path.
/// Deliberately NOT wired into this crate's own `cargo build` — this is one
/// integration test's setup step, not something every build of this crate
/// should pay for.
fn build_agent24d(checkout: &Path) -> PathBuf {
    let status = Command::new("cargo")
        .args(["build", "-p", "agent24d", "--bin", "agent24d"])
        .current_dir(checkout.join("rust"))
        .status()
        .expect("could not run cargo build for agent24d");
    assert!(status.success(), "cargo build -p agent24d failed");
    let bin = checkout.join("rust/target/debug/agent24d");
    assert!(
        bin.is_file(),
        "expected {} to exist after build",
        bin.display()
    );
    bin
}

/// `/tmp` directly, not `std::env::temp_dir()` — on macOS the latter
/// resolves through `/var/folders/<hash>/<hash>/T`, long enough to blow the
/// ~103-byte `sockaddr_un` budget the daemon's callback socket needs (same
/// reasoning, and same fix, as Sin90's own `tmp_home` and `agent24d`'s own
/// `me3f_blackbox.rs`).
fn tmp_home(tag: &str) -> PathBuf {
    let dir = Path::new("/tmp").join(format!("cos72-a24mount-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Install Cos72 as a real out-of-process package: an EXACT byte copy of
/// `domain-os.yml` (the kernel's `manifest_digest` check compares raw file
/// bytes, so a re-serialized or hand-typed copy that merely looks the same
/// would fail the digest match this test exists to catch) plus the
/// just-built `cos72` binary at the exact relative path the manifest's
/// `spawn.command: bin/cos72` names.
fn install_cos72(packages_root: &Path, cos72_bin: &Path) {
    let dir = packages_root.join("cos72");
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(
        dir.join("domain-os.yml"),
        include_bytes!("../domain-os.yml"),
    )
    .unwrap();
    std::fs::copy(cos72_bin, dir.join("bin/cos72")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(dir.join("bin/cos72"))
            .unwrap()
            .permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(dir.join("bin/cos72"), perm).unwrap();
    }
}

/// `daemon_token` is Agent24's own kernel bearer token — every `/api/v1/*`
/// route except `GET /api/v1/health` requires it, domain-OS routes
/// included. The kernel's proxy strips `Authorization` before handing the
/// request to Cos72, so Cos72 itself never sees it (Cos72's `/health` has no
/// gate of its own in T1.1.1 — the 401 this file checks is the KERNEL's,
/// proving the request genuinely went through the proxy rather than
/// somehow reaching Cos72 directly).
fn http_get(port: u16, daemon_token: Option<&str>, path: &str) -> Option<(u16, String)> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let auth = daemon_token
        .map(|t| format!("authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nhost: x\r\n{auth}connection: close\r\n\r\n"
    )
    .ok()?;
    let mut raw = String::new();
    s.read_to_string(&mut raw).ok()?;
    let status = raw.split(' ').nth(1)?.parse().ok()?;
    let resp_body = raw.split_once("\r\n\r\n").map(|(_, b)| b.to_owned())?;
    Some((status, resp_body))
}

/// Same wire-level reasoning as [`http_get`] — a real POST through the real
/// kernel proxy, so `cos72_task_routes_through_real_proxy` observes the
/// same 401-without-token boundary any other route does (docs/agent/tasks.md
/// T1.2.1 验收命令 #4: "只用真实 agent24d + 真实 cos72", not axum's own
/// in-process `oneshot`).
fn http_post(
    port: u16,
    daemon_token: Option<&str>,
    path: &str,
    body: &serde_json::Value,
) -> Option<(u16, String)> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let auth = daemon_token
        .map(|t| format!("authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let payload = body.to_string();
    write!(
        s,
        "POST {path} HTTP/1.1\r\nhost: x\r\n{auth}content-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{payload}",
        payload.len()
    )
    .ok()?;
    let mut raw = String::new();
    s.read_to_string(&mut raw).ok()?;
    let status = raw.split(' ').nth(1)?.parse().ok()?;
    let resp_body = raw.split_once("\r\n\r\n").map(|(_, b)| b.to_owned())?;
    Some((status, resp_body))
}

/// Graceful-first on every exit path (unwind included) — same reasoning as
/// Sin90's / Agent24's own `Running`: a plain `Child` drop does not
/// terminate the OS process, and an unconditional SIGKILL has been observed
/// to orphan a module blocked in `accept()`.
struct Running(std::process::Child);

impl Drop for Running {
    fn drop(&mut self) {
        #[allow(clippy::cast_possible_wrap)]
        if let Some(pid) = rustix::process::Pid::from_raw(self.0.id() as i32) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::Term);
            let by = Instant::now() + Duration::from_secs(10);
            while self.0.try_wait().is_ok_and(|s| s.is_none()) && Instant::now() < by {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Daemon {
    #[allow(dead_code)]
    run: Running,
    port: u16,
    token: String,
    stdout: Arc<Mutex<Vec<String>>>,
    stderr: Arc<Mutex<Vec<String>>>,
}

impl Daemon {
    fn combined_log(&self) -> String {
        let out = self.stdout.lock().unwrap().join("\n");
        let err = self.stderr.lock().unwrap().join("\n");
        format!("--- stdout ---\n{out}\n--- stderr ---\n{err}")
    }
}

fn drain_into(mut reader: impl std::io::Read + Send + 'static, sink: Arc<Mutex<Vec<String>>>) {
    std::thread::spawn(move || {
        let mut buf = BufReader::new(&mut reader);
        let mut line = String::new();
        loop {
            line.clear();
            match std::io::BufRead::read_line(&mut buf, &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => sink.lock().unwrap().push(line.trim_end().to_owned()),
            }
        }
    });
}

fn start_daemon(home: &Path, agent24d_bin: &Path) -> Daemon {
    let mut child = Command::new(agent24d_bin)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .args(["serve", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("could not spawn {}: {e}", agent24d_bin.display()));
    let mut stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let run = Running(child);

    // The ready line is the FIRST line of stdout.
    let mut first_line = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        match stdout.read(&mut byte) {
            Ok(1) => {
                if byte[0] == b'\n' {
                    break;
                }
                first_line.push(byte[0]);
            }
            _ => panic!("daemon closed stdout before printing a ready line"),
        }
    }
    let ready: serde_json::Value =
        serde_json::from_slice(&first_line).expect("the ready line must be JSON");

    let stdout_lines = Arc::new(Mutex::new(Vec::new()));
    let stderr_lines = Arc::new(Mutex::new(Vec::new()));
    drain_into(stdout, stdout_lines.clone());
    drain_into(stderr, stderr_lines.clone());

    Daemon {
        run,
        port: u16::try_from(ready["port"].as_u64().unwrap()).unwrap(),
        token: ready["token"].as_str().unwrap().to_owned(),
        stdout: stdout_lines,
        stderr: stderr_lines,
    }
}

fn os_list_entry(d: &Daemon, name: &str) -> serde_json::Value {
    let (status, body) =
        http_get(d.port, Some(&d.token), "/api/v1/os").expect("the daemon answered /api/v1/os");
    assert_eq!(status, 200, "{body}");
    let list: serde_json::Value = serde_json::from_str(&body).unwrap();
    list["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("{name} not in module list: {body}"))
}

fn spawn_ws_subscriber(port: u16, token: &str) -> std::sync::mpsc::Receiver<serde_json::Value> {
    use tokio_tungstenite::tungstenite;
    use tungstenite::client::IntoClientRequest;

    let (tx, rx) = std::sync::mpsc::channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let url = format!("ws://127.0.0.1:{port}/api/v1/events");
    let token = token.to_owned();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let mut request = url.into_client_request().expect("a valid ws:// url");
            request
                .headers_mut()
                .insert("Authorization", format!("Bearer {token}").parse().unwrap());
            let (mut socket, _) = tokio_tungstenite::connect_async(request)
                .await
                .expect("the real WS upgrade must succeed");
            let _ = ready_tx.send(());
            use futures::StreamExt;
            while let Some(Ok(tungstenite::Message::Text(text))) = socket.next().await {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                    continue;
                };
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
    });
    ready_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the WS subscriber never finished its upgrade");
    rx
}

/// docs/agent/tasks.md T1.1.1 验收命令 #5: a real `agent24d` mounts a real,
/// freshly-built `cos72` — `GET /api/v1/os` reports it `mounted`, `GET
/// /api/v1/cos72/health` answers 200 through the real proxy with
/// capabilities covering exactly `_a24/events/`, `_a24/memory/private/`,
/// `_a24/approval/` and NOT `_a24/scheduler/`, the WS boundary observes a
/// `module=cos72` `module.ready` event, and the same health request WITHOUT
/// the daemon token comes back 401 (负对照: proves the request really went
/// through the kernel, not straight to Cos72).
#[test]
#[ignore = "needs a sibling Agent24 checkout; run explicitly: cargo test --test agent24_mount_blackbox -- --ignored --test-threads=1"]
fn cos72_mounts_and_answers_health() {
    let checkout = agent24_checkout().unwrap_or_else(|| {
        panic!(
            "no Agent24 checkout found (set AGENT24_CHECKOUT or place it at ../Agent24) — this \
             test must FAIL, not silently skip, when its prerequisite is missing"
        )
    });
    let agent24d_bin = build_agent24d(&checkout);
    let cos72_bin = PathBuf::from(env!("CARGO_BIN_EXE_cos72"));

    let home = tmp_home("home");

    // First lifetime: nothing installed — proves the mount that follows is
    // caused by the install-then-restart sequence, not something else.
    let d1 = start_daemon(&home, &agent24d_bin);
    let (status, body) = http_get(d1.port, Some(&d1.token), "/api/v1/os").unwrap();
    assert_eq!(status, 200, "{body}");
    let before: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        before["modules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["name"] != "cos72"),
        "cos72 must not be mounted before it is installed: {body}"
    );
    drop(d1);

    install_cos72(&home.join(".agent24/packages"), &cos72_bin);

    // Second lifetime: real restart, same already-built agent24d binary, no
    // rebuild from here on for either binary.
    let d2 = start_daemon(&home, &agent24d_bin);
    let events = spawn_ws_subscriber(d2.port, &d2.token);

    // `os list` can report `mounted` slightly before the module has finished
    // handshaking and is actually ready to serve a proxied request — retry
    // the real HTTP call rather than gating on the list alone (same race
    // Sin90's own harness hits and handles the same way).
    let deadline = Instant::now() + Duration::from_secs(30);
    let health = loop {
        if let Some((status, body)) = http_get(d2.port, Some(&d2.token), "/api/v1/cos72/health") {
            if status == 200 || Instant::now() >= deadline {
                assert_eq!(status, 200, "daemon log:\n{}", d2.combined_log());
                break body;
            }
        }
        assert!(
            Instant::now() < deadline,
            "cos72 never answered /health through the real proxy; daemon log:\n{}",
            d2.combined_log()
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(os_list_entry(&d2, "cos72")["state"], "mounted");

    let health: serde_json::Value = serde_json::from_str(&health).unwrap();
    let capabilities: Vec<String> = health["capabilities"]
        .as_array()
        .unwrap_or_else(|| panic!("health response has no capabilities array: {health}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    for want in ["_a24/events/", "_a24/memory/private/", "_a24/approval/"] {
        assert!(
            capabilities.iter().any(|c| c == want),
            "capabilities {capabilities:?} must include {want}"
        );
    }
    assert!(
        !capabilities.iter().any(|c| c == "_a24/scheduler/"),
        "capabilities {capabilities:?} must NOT include _a24/scheduler/ (docs/agent/\
         architecture.md 核心判断 3: Cos72 does not request scheduler)"
    );

    // The WS boundary observed a `module=cos72` `module.ready` event — the
    // subscriber started before the restart's handshake, so it sees it.
    let overall_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let remaining = overall_deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "never observed cos72's module.ready event on the real WS boundary; daemon log:\n{}",
            d2.combined_log()
        );
        match events.recv_timeout(remaining.min(Duration::from_secs(5))) {
            Ok(event)
                if event["type"] == "module"
                    && event["payload"]["module"] == "cos72"
                    && event["payload"]["kind"] == "module.ready" =>
            {
                break;
            }
            Ok(_) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!(
                    "WS subscriber thread ended; daemon log:\n{}",
                    d2.combined_log()
                )
            }
        }
    }

    // 负对照: the same route WITHOUT the daemon token must be rejected by the
    // KERNEL (401) — proves the 200 above really went through the proxy.
    let (status, body) = http_get(d2.port, None, "/api/v1/cos72/health")
        .unwrap_or_else(|| panic!("no response from GET /health without a token"));
    assert_eq!(
        status, 401,
        "GET /health with no daemon token must be rejected by the kernel: {body}"
    );
}

/// Wait (up to `secs`) for `GET path` through the real proxy to return the
/// given status, retrying on connection hiccups the same way every other
/// polling loop in this file does; returns the parsed JSON body.
fn wait_for_get(d: &Daemon, path: &str, want_status: u16, secs: u64) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some((status, body)) = http_get(d.port, Some(&d.token), path) {
            if status == want_status {
                return serde_json::from_str(&body)
                    .unwrap_or_else(|e| panic!("GET {path} returned non-JSON body {body:?}: {e}"));
            }
            if Instant::now() >= deadline {
                panic!(
                    "GET {path} never returned {want_status} (last: {status} {body}); daemon \
                     log:\n{}",
                    d.combined_log()
                );
            }
        }
        assert!(
            Instant::now() < deadline,
            "GET {path} got no response before the deadline; daemon log:\n{}",
            d.combined_log()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Polls the `test-hooks`-only `POST /debug/memory-recall` route (spec.md
/// 「REST 路由」) until it reports at least `want_len` items for `query`, or
/// panics at the deadline — the memory_pump only drains `outbox` on its own
/// 2s tick (`workers::memory_pump::PUMP_INTERVAL`), so this is a genuine
/// wait, not an immediate check.
fn wait_for_memory_recall(
    d: &Daemon,
    query: &str,
    want_len: usize,
    secs: u64,
) -> serde_json::Value {
    let path = "/api/v1/cos72/debug/memory-recall";
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some((status, body)) = http_post(
            d.port,
            Some(&d.token),
            path,
            &serde_json::json!({"query": query}),
        ) {
            if status == 200 {
                let value: serde_json::Value = serde_json::from_str(&body)
                    .unwrap_or_else(|e| panic!("POST {path} returned non-JSON body {body:?}: {e}"));
                let len = value["items"].as_array().map_or(0, Vec::len);
                if len >= want_len {
                    return value;
                }
            } else {
                assert!(
                    Instant::now() < deadline,
                    "POST {path} never returned 200 (last: {status} {body}); daemon log:\n{}",
                    d.combined_log()
                );
            }
        }
        assert!(
            Instant::now() < deadline,
            "POST {path} never reported >= {want_len} item(s) for {query:?} before the \
             deadline; daemon log:\n{}",
            d.combined_log()
        );
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// docs/agent/tasks.md T1.3.1 验收命令 #6 (T1.3.1a slice — the points half;
/// T1.3.1b adds the memory-recall assertions on top of this same scenario):
/// a real `agent24d` + real `cos72`, submit → the kernel's own
/// `/api/v1/module-approvals` shows a `module=cos72`, `kind=advise` row whose
/// `payload.award_id` matches the submitted award → approving it credits the
/// ledger within the poller's own interval → `GET /points/{member}` equals
/// the reward → a SECOND task's award, denied instead, returns its task to
/// `claimed` and does NOT move the balance (正对照) → restarting the daemon
/// (a fresh generation, fresh poller) leaves the balance unchanged (pure
/// ledger replay, nothing cached in memory).
#[test]
#[ignore = "needs a sibling Agent24 checkout; run explicitly: cargo test --test agent24_mount_blackbox -- --ignored --test-threads=1"]
fn cos72_award_roundtrip_under_real_daemon() {
    let checkout = agent24_checkout().unwrap_or_else(|| {
        panic!(
            "no Agent24 checkout found (set AGENT24_CHECKOUT or place it at ../Agent24) — this \
             test must FAIL, not silently skip, when its prerequisite is missing"
        )
    });
    let agent24d_bin = build_agent24d(&checkout);
    let cos72_bin = PathBuf::from(env!("CARGO_BIN_EXE_cos72"));

    let home = tmp_home("award-roundtrip");
    install_cos72(&home.join(".agent24/packages"), &cos72_bin);

    let daemon = start_daemon(&home, &agent24d_bin);

    let ready_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some((200, _)) = http_get(daemon.port, Some(&daemon.token), "/api/v1/cos72/health") {
            break;
        }
        assert!(
            Instant::now() < ready_deadline,
            "cos72 never answered /health through the real proxy; daemon log:\n{}",
            daemon.combined_log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // ---- Path 1: approved → credited ----
    let publish_body =
        serde_json::json!({"title": "approved task", "reward_points": 7, "publisher": "pub1"});
    let (status, body) = http_post(
        daemon.port,
        Some(&daemon.token),
        "/api/v1/cos72/tasks",
        &publish_body,
    )
    .unwrap();
    assert_eq!(
        status,
        201,
        "daemon log:\n{}\nbody: {body}",
        daemon.combined_log()
    );
    let task: serde_json::Value = serde_json::from_str(&body).unwrap();
    let task_id = task["task_id"].as_str().unwrap().to_owned();

    let (status, _) = http_post(
        daemon.port,
        Some(&daemon.token),
        &format!("/api/v1/cos72/tasks/{task_id}/claim"),
        &serde_json::json!({"member": "mem1"}),
    )
    .unwrap();
    assert_eq!(status, 200, "daemon log:\n{}", daemon.combined_log());

    let (status, body) = http_post(
        daemon.port,
        Some(&daemon.token),
        &format!("/api/v1/cos72/tasks/{task_id}/submit"),
        &serde_json::json!({"member": "mem1"}),
    )
    .unwrap();
    assert_eq!(
        status,
        202,
        "daemon log:\n{}\nbody: {body}",
        daemon.combined_log()
    );
    let submitted: serde_json::Value = serde_json::from_str(&body).unwrap();
    let award_id = submitted["award"]["award_id"].as_str().unwrap().to_owned();
    assert!(
        submitted["award"]["approval_id"].is_string(),
        "a real advise must have returned a real approval_id: {submitted}"
    );

    // Find the pending module-approval the real kernel recorded for this
    // award — proves advise really reached the kernel's own approval store,
    // not just a mock.
    let pending = wait_for_get(
        &daemon,
        "/api/v1/module-approvals?decision=pending",
        200,
        10,
    );
    let approval = pending["module_approvals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["module"] == "cos72" && a["payload"]["award_id"] == award_id)
        .unwrap_or_else(|| panic!("no pending module-approval for award {award_id}: {pending}"));
    assert_eq!(approval["kind"], "advise", "{approval}");
    let approval_id = approval["id"].as_str().unwrap().to_owned();

    let decide_body = serde_json::json!({"decision": "approved"});
    let (status, body) = http_post(
        daemon.port,
        Some(&daemon.token),
        &format!("/api/v1/module-approvals/{approval_id}"),
        &decide_body,
    )
    .unwrap();
    assert_eq!(
        status,
        200,
        "daemon log:\n{}\nbody: {body}",
        daemon.combined_log()
    );

    // The poller ticks every 3s (docs/agent/spec.md「入账」) — give it up to
    // 30s of margin under test/CI load.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let task = wait_for_get(&daemon, &format!("/api/v1/cos72/tasks/{task_id}"), 200, 5);
        if task["status"] == "completed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "task {task_id} never reached completed after approval; last: {task}; daemon \
             log:\n{}",
            daemon.combined_log()
        );
        std::thread::sleep(Duration::from_millis(300));
    }
    let points = wait_for_get(&daemon, "/api/v1/cos72/points/mem1", 200, 5);
    assert_eq!(points["balance"], 7, "{points}");

    // ---- Memory (T1.3.1b): the completion summary lands via outbox + the
    // memory_pump — poll the test-hooks debug route until the pump (2s tick)
    // has drained the row (spec.md「记忆泵」dedup_key = cos72:task:<id>:
    // completed).
    let dedup_key = format!("cos72:task:{task_id}:completed");
    let recall = wait_for_memory_recall(&daemon, &dedup_key, 1, 20);
    let items = recall["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{recall}");
    assert_eq!(items[0]["body"]["task_id"], task_id, "{recall}");
    assert_eq!(items[0]["body"]["award_id"], award_id, "{recall}");
    assert_eq!(items[0]["body"]["dedup_key"], dedup_key, "{recall}");

    // ---- Path 2 (正对照): denied → back to claimed, balance unchanged ----
    let publish_body2 =
        serde_json::json!({"title": "denied task", "reward_points": 99, "publisher": "pub1"});
    let (status, body) = http_post(
        daemon.port,
        Some(&daemon.token),
        "/api/v1/cos72/tasks",
        &publish_body2,
    )
    .unwrap();
    assert_eq!(status, 201, "{body}");
    let task2: serde_json::Value = serde_json::from_str(&body).unwrap();
    let task2_id = task2["task_id"].as_str().unwrap().to_owned();
    http_post(
        daemon.port,
        Some(&daemon.token),
        &format!("/api/v1/cos72/tasks/{task2_id}/claim"),
        &serde_json::json!({"member": "mem2"}),
    )
    .unwrap();
    let (status, body) = http_post(
        daemon.port,
        Some(&daemon.token),
        &format!("/api/v1/cos72/tasks/{task2_id}/submit"),
        &serde_json::json!({"member": "mem2"}),
    )
    .unwrap();
    assert_eq!(status, 202, "{body}");
    let submitted2: serde_json::Value = serde_json::from_str(&body).unwrap();
    let award2_id = submitted2["award"]["award_id"].as_str().unwrap().to_owned();

    let pending2 = wait_for_get(
        &daemon,
        "/api/v1/module-approvals?decision=pending",
        200,
        10,
    );
    let approval2 = pending2["module_approvals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["module"] == "cos72" && a["payload"]["award_id"] == award2_id)
        .unwrap_or_else(|| panic!("no pending module-approval for award {award2_id}: {pending2}"));
    let approval2_id = approval2["id"].as_str().unwrap().to_owned();
    let (status, body) = http_post(
        daemon.port,
        Some(&daemon.token),
        &format!("/api/v1/module-approvals/{approval2_id}"),
        &serde_json::json!({"decision": "denied"}),
    )
    .unwrap();
    assert_eq!(status, 200, "{body}");

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let task2 = wait_for_get(&daemon, &format!("/api/v1/cos72/tasks/{task2_id}"), 200, 5);
        if task2["status"] == "claimed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "task {task2_id} never returned to claimed after denial; last: {task2}; daemon \
             log:\n{}",
            daemon.combined_log()
        );
        std::thread::sleep(Duration::from_millis(300));
    }
    let points_after_deny = wait_for_get(&daemon, "/api/v1/cos72/points/mem1", 200, 5);
    assert_eq!(
        points_after_deny["balance"], 7,
        "a denied award must never move any balance: {points_after_deny}"
    );
    let points_mem2 = wait_for_get(&daemon, "/api/v1/cos72/points/mem2", 200, 5);
    assert_eq!(points_mem2["balance"], 0, "{points_mem2}");

    // ---- Restart: balance survives (pure ledger replay) ----
    drop(daemon);
    let daemon2 = start_daemon(&home, &agent24d_bin);
    let ready_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some((200, _)) = http_get(daemon2.port, Some(&daemon2.token), "/api/v1/cos72/health")
        {
            break;
        }
        assert!(
            Instant::now() < ready_deadline,
            "cos72 never answered /health after restart; daemon log:\n{}",
            daemon2.combined_log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let points_restarted = wait_for_get(&daemon2, "/api/v1/cos72/points/mem1", 200, 10);
    assert_eq!(
        points_restarted["balance"], 7,
        "balance must survive a restart unchanged: {points_restarted}"
    );

    // Memory also survives the restart unchanged — it lives in the kernel's
    // own on-disk store (same `home`), independent of Cos72's own process
    // lifetime; still exactly one recollection for this dedup_key (no
    // re-remembering on the new generation's own pump startup).
    let recall_after_restart = wait_for_memory_recall(&daemon2, &dedup_key, 1, 15);
    assert_eq!(
        recall_after_restart["items"].as_array().unwrap().len(),
        1,
        "memory must survive a restart unchanged, not be re-written: {recall_after_restart}"
    );
}

/// docs/agent/tasks.md T1.2.1 验收命令 #4: a real `agent24d` + real `cos72`,
/// publish → claim → submit through the real proxy, `GET /tasks/{id}` ends up
/// `submitted`, and the WS boundary observes `task.published`, `task.claimed`,
/// `task.submitted` (each `module=cos72`, carrying this `task_id`) in order.
#[test]
#[ignore = "needs a sibling Agent24 checkout; run explicitly: cargo test --test agent24_mount_blackbox -- --ignored --test-threads=1"]
fn cos72_task_routes_through_real_proxy() {
    let checkout = agent24_checkout().unwrap_or_else(|| {
        panic!(
            "no Agent24 checkout found (set AGENT24_CHECKOUT or place it at ../Agent24) — this \
             test must FAIL, not silently skip, when its prerequisite is missing"
        )
    });
    let agent24d_bin = build_agent24d(&checkout);
    let cos72_bin = PathBuf::from(env!("CARGO_BIN_EXE_cos72"));

    let home = tmp_home("task-routes");
    install_cos72(&home.join(".agent24/packages"), &cos72_bin);

    let daemon = start_daemon(&home, &agent24d_bin);
    let events = spawn_ws_subscriber(daemon.port, &daemon.token);

    // Same "wait for the real proxy, not just the module list" race as
    // `cos72_mounts_and_answers_health` above.
    let ready_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some((200, _)) = http_get(daemon.port, Some(&daemon.token), "/api/v1/cos72/health") {
            break;
        }
        assert!(
            Instant::now() < ready_deadline,
            "cos72 never answered /health through the real proxy; daemon log:\n{}",
            daemon.combined_log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let publish_body =
        serde_json::json!({"title": "real mount task", "reward_points": 10, "publisher": "pub1"});
    let (status, body) = http_post(
        daemon.port,
        Some(&daemon.token),
        "/api/v1/cos72/tasks",
        &publish_body,
    )
    .unwrap_or_else(|| panic!("no response from POST /tasks"));
    assert_eq!(
        status,
        201,
        "daemon log:\n{}\nbody: {body}",
        daemon.combined_log()
    );
    let task: serde_json::Value = serde_json::from_str(&body).unwrap();
    let task_id = task["task_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no task_id in publish response: {body}"))
        .to_owned();

    let (status, body) = http_post(
        daemon.port,
        Some(&daemon.token),
        &format!("/api/v1/cos72/tasks/{task_id}/claim"),
        &serde_json::json!({"member": "mem1"}),
    )
    .unwrap_or_else(|| panic!("no response from POST /claim"));
    assert_eq!(
        status,
        200,
        "daemon log:\n{}\nbody: {body}",
        daemon.combined_log()
    );

    let (status, body) = http_post(
        daemon.port,
        Some(&daemon.token),
        &format!("/api/v1/cos72/tasks/{task_id}/submit"),
        &serde_json::json!({"member": "mem1"}),
    )
    .unwrap_or_else(|| panic!("no response from POST /submit"));
    assert_eq!(
        status,
        202,
        "daemon log:\n{}\nbody: {body}",
        daemon.combined_log()
    );

    let (status, body) = http_get(
        daemon.port,
        Some(&daemon.token),
        &format!("/api/v1/cos72/tasks/{task_id}"),
    )
    .unwrap_or_else(|| panic!("no response from GET /tasks/{{id}}"));
    assert_eq!(
        status,
        200,
        "daemon log:\n{}\nbody: {body}",
        daemon.combined_log()
    );
    let task: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(task["status"], "submitted", "{body}");

    // WS boundary observed the three events, in order, each carrying this
    // task_id — the subscriber started before any of the three calls above.
    let expected_kinds = ["task.published", "task.claimed", "task.submitted"];
    let mut next = 0;
    let overall_deadline = Instant::now() + Duration::from_secs(30);
    while next < expected_kinds.len() {
        let remaining = overall_deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "never observed {:?} on the real WS boundary (saw {next} of {}); daemon log:\n{}",
            expected_kinds[next],
            expected_kinds.len(),
            daemon.combined_log()
        );
        match events.recv_timeout(remaining.min(Duration::from_secs(5))) {
            Ok(event)
                if event["type"] == "module"
                    && event["payload"]["module"] == "cos72"
                    && event["payload"]["kind"] == expected_kinds[next]
                    && event["payload"]["payload"]["task_id"] == task_id =>
            {
                next += 1;
            }
            Ok(_) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!(
                    "WS subscriber thread ended after {next} of {} events; daemon log:\n{}",
                    expected_kinds.len(),
                    daemon.combined_log()
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// T1.4.1 (ME4-5.3.4, docs/agent/tasks.md) — real coexistence with Sin90.
// Everything below this point is shared only by the two tests at the very
// end of this file; every test above predates T1.4.1 and mounts Cos72 alone.
// ---------------------------------------------------------------------------

/// Locate a sibling Sin90 checkout already migrated to `agent24-os-sdk`
/// (ME4-5.2.1). Deliberately NO default guess (unlike [`agent24_checkout`]):
/// docs/agent/tasks.md T1.4.1 验收命令 #3 — "前置缺失即失败：不设
/// `SIN90_CHECKOUT` 时该测试 panic（不是 pass/skip）" — a default path would
/// let a test silently pass (or silently skip) against nothing, and a stale
/// `PathBuf::exists()`-guarded default could also silently start mounting an
/// UNRELATED directory that happens to contain a `Cargo.toml`. `unwrap` (not
/// `unwrap_or_else`) on the env var read: any read failure (unset OR not
/// valid Unicode) must be the same hard panic.
fn sin90_checkout() -> PathBuf {
    let dir = PathBuf::from(std::env::var("SIN90_CHECKOUT").unwrap_or_else(|_| {
        panic!(
            "SIN90_CHECKOUT env var must be set to a sibling Sin90 checkout already migrated \
             to agent24-os-sdk (ME4-5.2.1) — this test must FAIL, not silently skip, when its \
             prerequisite is missing (docs/agent/tasks.md T1.4.1 验收命令 #3)"
        )
    }));
    assert!(
        dir.join("Cargo.toml").is_file(),
        "SIN90_CHECKOUT={} has no Cargo.toml at its root",
        dir.display()
    );
    dir
}

/// Build Sin90 with its own `test-hooks` feature (needed for its `POST
/// /debug/kernel-roundtrip` route, which both T1.4.1 tests below use as
/// Sin90's own memory-recall probe). Mirrors Sin90's own
/// `build_sin90_with_test_hooks` — a DEDICATED `--target-dir` under the
/// Sin90 checkout itself, not this crate's `target/`, so this build uses
/// Sin90's own `Cargo.lock`/dependency graph exactly as Sin90's own CI would,
/// and does not collide with anything this crate's own `cargo test` builds.
fn build_sin90_with_test_hooks(checkout: &Path) -> PathBuf {
    let target_dir = checkout.join("target/test-hooks-debug");
    let status = Command::new("cargo")
        .args([
            "build",
            "--bin",
            "sin90",
            "--features",
            "test-hooks",
            "--target-dir",
        ])
        .arg(&target_dir)
        .current_dir(checkout)
        .status()
        .expect("could not run cargo build for sin90 (test-hooks)");
    assert!(
        status.success(),
        "cargo build --features test-hooks --bin sin90 (in {}) failed",
        checkout.display()
    );
    let bin = target_dir.join("debug/sin90");
    assert!(
        bin.is_file(),
        "expected {} to exist after build",
        bin.display()
    );
    bin
}

/// Install Sin90 as a real out-of-process package next to Cos72's own, in
/// the SAME `packages_root` (proving both can share one real `agent24d`
/// install — T1.4.1's whole point). Unlike Sin90's own `install_sin90`
/// (which lives INSIDE the Sin90 crate and can `include_bytes!("../domain-os.yml")`
/// at compile time), this crate is not Sin90's crate — `domain-os.yml` is
/// read from `sin90_checkout` at RUNTIME instead. The kernel's own
/// `manifest_digest` check still compares raw file bytes
/// (`agent24-os-packages::discovery`), so this must still be an exact byte
/// copy of Sin90's real, checked-in manifest, not a hand-typed one — reading
/// the real file off disk is what guarantees that, the same guarantee
/// `include_bytes!` gives Sin90's own test, just obtained a different way.
fn install_sin90(packages_root: &Path, sin90_checkout: &Path, sin90_bin: &Path) {
    let dir = packages_root.join("sin90");
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let manifest = std::fs::read(sin90_checkout.join("domain-os.yml")).unwrap_or_else(|e| {
        panic!(
            "could not read {}/domain-os.yml: {e}",
            sin90_checkout.display()
        )
    });
    std::fs::write(dir.join("domain-os.yml"), manifest).unwrap();
    std::fs::copy(sin90_bin, dir.join("bin/sin90")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(dir.join("bin/sin90"))
            .unwrap()
            .permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(dir.join("bin/sin90"), perm).unwrap();
    }
}

/// A general HTTP call over the real proxy, with an OPTIONAL actor-key
/// header — unlike [`http_get`]/[`http_post`] (Cos72's own routes need no
/// actor key at all, Q1's own recorded decision), Sin90's routes are gated
/// by `x-sin90-actor-key` (`http::actor::ActorKeys`), and this test drives
/// several of them (`/reviews`, `/reviews/{id}` PATCH, `/reviews/{id}/finalize`,
/// `/routines`, `/debug/kernel-roundtrip`). Mirrors Sin90's own `http_call`.
fn http_call(
    port: u16,
    method: &str,
    path: &str,
    daemon_token: Option<&str>,
    actor_key: Option<&str>,
    body: Option<&str>,
) -> Option<(u16, String)> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let auth = daemon_token
        .map(|t| format!("authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let actor = actor_key
        .map(|k| format!("x-sin90-actor-key: {k}\r\n"))
        .unwrap_or_default();
    let body = body.unwrap_or("");
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nhost: x\r\n{auth}{actor}content-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .ok()?;
    let mut raw = String::new();
    s.read_to_string(&mut raw).ok()?;
    let status = raw.split(' ').nth(1)?.parse().ok()?;
    let resp_body = raw.split_once("\r\n\r\n").map(|(_, b)| b.to_owned())?;
    Some((status, resp_body))
}

/// Every file named `name` found anywhere under `dir` (depth-first) — used
/// only to find Sin90's `actor-keys.json` without hard-coding Agent24's own
/// data-dir layout, same reasoning as Sin90's own `find_file`.
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|n| n == name) {
            return Some(path);
        }
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        }
    }
    None
}

impl Daemon {
    /// Sin90's own human actor key, persisted to `<A24_DATA_DIR>/actor-keys.json`
    /// under this test's isolated `HOME` (mirrors Sin90's own
    /// `Daemon::read_actor_key` — Agent24's module launch passes only a
    /// fixed env allowlist, so a `SIN90_*` env var cannot reach a mounted
    /// module). Also asserts the raw key never leaked into the daemon log
    /// (Agent24 re-logs every module output line).
    fn read_actor_key(&self, home: &Path, which: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        let path = loop {
            if let Some(p) = find_file(home, "actor-keys.json") {
                break p;
            }
            assert!(
                Instant::now() < deadline,
                "sin90 never created actor-keys.json under {}; daemon log:\n{}",
                home.display(),
                self.combined_log()
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        let keys: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let key = keys[which]
            .as_str()
            .unwrap_or_else(|| panic!("no {which:?} key in {}: {keys}", path.display()))
            .to_owned();
        assert!(
            !self.combined_log().contains(&key),
            "the raw {which} actor key leaked into the daemon log"
        );
        key
    }
}

/// Polls `GET /api/v1/cos72/health` through the real proxy until it answers
/// 200 — the same "mounted in `/api/v1/os` can race ahead of ready to serve
/// a proxied request" race every other Cos72 test in this file retries
/// around inline; factored out here because both T1.4.1 tests need it.
fn wait_for_cos72_ready(d: &Daemon, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some((200, _)) = http_get(d.port, Some(&d.token), "/api/v1/cos72/health") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "cos72 never answered /health through the real proxy; daemon log:\n{}",
            d.combined_log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Same race, for Sin90's side: `os list` reporting `mounted` then Sin90's
/// own side-effect-free `GET /today` actually answering (Sin90's own
/// `wait_for_sin90_ready` in its own `tests/agent24_mount_blackbox.rs`).
fn wait_for_sin90_ready(d: &Daemon, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if os_list_entry(d, "sin90")["state"] == "mounted" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "sin90 never reached state \"mounted\"; daemon log:\n{}",
            d.combined_log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let deadline = Instant::now() + timeout;
    loop {
        if let Some((200, _)) = http_get(d.port, Some(&d.token), "/api/v1/sin90/today") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "sin90 never answered /today through the real proxy; daemon log:\n{}",
            d.combined_log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The KERNEL's own `GET /api/v1/schedules` — a top-level kernel route, not
/// proxied to either module, gated only by the daemon bearer token (same as
/// `GET /api/v1/os`). docs/agent/tasks.md T1.4.1 own note: "调度隔离从内核
/// REST 侧验，是 ME4-S3 §8 Q4 明确允许的路径" — this is that REST side.
fn kernel_schedules(d: &Daemon) -> serde_json::Value {
    let (status, body) = http_get(d.port, Some(&d.token), "/api/v1/schedules")
        .unwrap_or_else(|| panic!("no response from GET /api/v1/schedules"));
    assert_eq!(status, 200, "{body}");
    serde_json::from_str(&body).unwrap()
}

/// `POST /api/v1/sin90/routines` with the human actor key, through the real
/// proxy — mirrors Sin90's own `create_routine`.
fn create_routine(d: &Daemon, human_key: &str, body: &str) -> serde_json::Value {
    let (status, resp) = http_call(
        d.port,
        "POST",
        "/api/v1/sin90/routines",
        Some(&d.token),
        Some(human_key),
        Some(body),
    )
    .unwrap();
    assert_eq!(status, 201, "POST /routines: {resp}");
    serde_json::from_str(&resp).unwrap()
}

/// Polls `GET /api/v1/schedules` until at least one row is owned by `module`
/// — Sin90's own Routine→kernel sync goes through its outbox/reconciler
/// (Sin90's own `routine_m3_real_mount_acceptance` polls the identical way,
/// `wait_for_one_schedule_row`), not synchronously with the `POST /routines`
/// call that creates it.
fn wait_for_any_schedule_row_owned_by(
    d: &Daemon,
    module: &str,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    loop {
        let schedules = kernel_schedules(d);
        if let Some(row) = schedules["schedules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["owner"]["module"] == module)
        {
            return row.clone();
        }
        assert!(
            Instant::now() < deadline,
            "no schedule row owned by {module:?} appeared within {timeout:?}; last: {schedules}; \
             daemon log:\n{}",
            d.combined_log()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Runs Cos72's ordinary task → award → advise → approve → credited → memory
/// flow to completion and returns the resulting `cos72:task:<id>:completed`
/// dedup_key — shared by both T1.4.1 tests below (`cos72_full_flow_real_mount`'s
/// own "does the flow still work when Sin90 is co-mounted" check, and
/// `cos72_and_sin90_coexist_isolated`'s own "Cos72 can find ITS OWN marker"
/// positive control) instead of duplicating the publish/claim/submit/approve
/// sequence `cos72_award_roundtrip_under_real_daemon` already exercises for
/// Cos72 alone. `tag` disambiguates the task title/publisher/member across
/// calls against the SAME daemon.
fn cos72_complete_one_task(daemon: &Daemon, tag: &str) -> String {
    let member = format!("mem-{tag}");
    let publish_body = serde_json::json!({
        "title": format!("coexist {tag}"),
        "reward_points": 5,
        "publisher": format!("pub-{tag}"),
    });
    let (status, body) = http_post(
        daemon.port,
        Some(&daemon.token),
        "/api/v1/cos72/tasks",
        &publish_body,
    )
    .unwrap_or_else(|| panic!("no response from POST /tasks"));
    assert_eq!(
        status,
        201,
        "daemon log:\n{}\nbody: {body}",
        daemon.combined_log()
    );
    let task: serde_json::Value = serde_json::from_str(&body).unwrap();
    let task_id = task["task_id"].as_str().unwrap().to_owned();

    let (status, body) = http_post(
        daemon.port,
        Some(&daemon.token),
        &format!("/api/v1/cos72/tasks/{task_id}/claim"),
        &serde_json::json!({"member": member}),
    )
    .unwrap();
    assert_eq!(
        status,
        200,
        "daemon log:\n{}\nbody: {body}",
        daemon.combined_log()
    );

    let (status, body) = http_post(
        daemon.port,
        Some(&daemon.token),
        &format!("/api/v1/cos72/tasks/{task_id}/submit"),
        &serde_json::json!({"member": member}),
    )
    .unwrap();
    assert_eq!(
        status,
        202,
        "daemon log:\n{}\nbody: {body}",
        daemon.combined_log()
    );
    let submitted: serde_json::Value = serde_json::from_str(&body).unwrap();
    let award_id = submitted["award"]["award_id"].as_str().unwrap().to_owned();

    let pending = wait_for_get(daemon, "/api/v1/module-approvals?decision=pending", 200, 10);
    let approval = pending["module_approvals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["module"] == "cos72" && a["payload"]["award_id"] == award_id)
        .unwrap_or_else(|| panic!("no pending module-approval for award {award_id}: {pending}"));
    let approval_id = approval["id"].as_str().unwrap().to_owned();

    let (status, body) = http_post(
        daemon.port,
        Some(&daemon.token),
        &format!("/api/v1/module-approvals/{approval_id}"),
        &serde_json::json!({"decision": "approved"}),
    )
    .unwrap();
    assert_eq!(
        status,
        200,
        "daemon log:\n{}\nbody: {body}",
        daemon.combined_log()
    );

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let task = wait_for_get(daemon, &format!("/api/v1/cos72/tasks/{task_id}"), 200, 5);
        if task["status"] == "completed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "task {task_id} never reached completed after approval; daemon log:\n{}",
            daemon.combined_log()
        );
        std::thread::sleep(Duration::from_millis(300));
    }

    let dedup_key = format!("cos72:task:{task_id}:completed");
    let recall = wait_for_memory_recall(daemon, &dedup_key, 1, 20);
    assert_eq!(recall["items"].as_array().unwrap().len(), 1, "{recall}");
    dedup_key
}

/// docs/agent/tasks.md T1.4.1 目标: "全流程...两模块同时挂载时..." — the
/// positive half. `cos72_award_roundtrip_under_real_daemon` (T1.3.1b)
/// already proves Cos72's full task/award/approval/memory flow end to end
/// WITH Cos72 mounted alone, including the denied path and a restart. This
/// test does not repeat that coverage; its only new fact is that the SAME
/// happy-path flow (and a restart) still completes correctly with a SECOND
/// real out-of-process module (Sin90) mounted on the SAME kernel at the SAME
/// time — proving the isolation assertions in `cos72_and_sin90_coexist_isolated`
/// below are checking a system that otherwise still works, not one where
/// Cos72 was simply broken/starved by sharing a daemon.
#[test]
#[ignore = "needs sibling Agent24 + Sin90 checkouts; run explicitly: AGENT24_CHECKOUT=... SIN90_CHECKOUT=... cargo test --features test-hooks --test agent24_mount_blackbox -- --ignored --test-threads=1"]
fn cos72_full_flow_real_mount() {
    let checkout = agent24_checkout().unwrap_or_else(|| {
        panic!(
            "no Agent24 checkout found (set AGENT24_CHECKOUT or place it at ../Agent24) — this \
             test must FAIL, not silently skip, when its prerequisite is missing"
        )
    });
    let sin90_checkout = sin90_checkout();
    let agent24d_bin = build_agent24d(&checkout);
    let cos72_bin = PathBuf::from(env!("CARGO_BIN_EXE_cos72"));
    let sin90_bin = build_sin90_with_test_hooks(&sin90_checkout);

    let home = tmp_home("full-flow-coexist");
    install_cos72(&home.join(".agent24/packages"), &cos72_bin);
    install_sin90(&home.join(".agent24/packages"), &sin90_checkout, &sin90_bin);

    let daemon = start_daemon(&home, &agent24d_bin);
    wait_for_cos72_ready(&daemon, Duration::from_secs(30));
    wait_for_sin90_ready(&daemon, Duration::from_secs(30));
    assert_eq!(
        os_list_entry(&daemon, "cos72")["state"],
        "mounted",
        "daemon log:\n{}",
        daemon.combined_log()
    );
    assert_eq!(
        os_list_entry(&daemon, "sin90")["state"],
        "mounted",
        "daemon log:\n{}",
        daemon.combined_log()
    );

    let dedup_key = cos72_complete_one_task(&daemon, "full-flow");
    let balance = wait_for_get(&daemon, "/api/v1/cos72/points/mem-full-flow", 200, 5);
    assert_eq!(balance["balance"], 5, "{balance}");

    // ---- restart: both modules re-mount, balance + memory both survive ----
    drop(daemon);
    let daemon2 = start_daemon(&home, &agent24d_bin);
    wait_for_cos72_ready(&daemon2, Duration::from_secs(30));
    wait_for_sin90_ready(&daemon2, Duration::from_secs(30));
    let balance_after_restart =
        wait_for_get(&daemon2, "/api/v1/cos72/points/mem-full-flow", 200, 10);
    assert_eq!(
        balance_after_restart["balance"], 5,
        "balance must survive a restart with sin90 co-mounted unchanged: {balance_after_restart}"
    );
    let recall_after_restart = wait_for_memory_recall(&daemon2, &dedup_key, 1, 15);
    assert_eq!(
        recall_after_restart["items"].as_array().unwrap().len(),
        1,
        "memory must survive a restart with sin90 co-mounted unchanged, not be re-written: \
         {recall_after_restart}"
    );
}

/// docs/agent/tasks.md T1.4.1 验收命令 #2 — the actual isolation contract,
/// under ONE real `agent24d` with a real `cos72` AND a real `sin90` mounted
/// at the same time:
///
/// - `GET /api/v1/os`: both `mounted`.
/// - Memory: each module's own recall finds its OWN marker/dedup_key (正对照,
///   ≥ 1); Cos72's `/debug/memory-recall` for Sin90's own fixed debug-route
///   marker (`kernel_roundtrip.rs`'s `"t3.2.3-kernel-clients-roundtrip"` —
///   this test's `M_s`) → 0; Sin90's own debug route, asked to recall Cos72's
///   completion dedup_key → 0.
/// - Schedules: a Sin90 Routine produces an `owner_module=sin90` row in the
///   KERNEL's own `GET /api/v1/schedules` (正对照); `owner_module=cos72`
///   stays at 0 — the structural reason being Cos72's manifest never
///   requests `scheduler` at all (re-checked here directly against
///   `/health`'s own capabilities, docs/agent/tasks.md T1.4.1's own note:
///   "Cos72 `/health` 的 capabilities 不含 `_a24/scheduler/`").
///
/// docs/agent/tasks.md T1.4.1 验收命令 #3 ("前置缺失即失败：不设
/// `SIN90_CHECKOUT` 时该测试 panic") is [`sin90_checkout`]'s own behavior,
/// exercised by every test in this section — not re-tested as its own
/// `#[test]` here (a test that panics under its own normal, `SIN90_CHECKOUT`-set
/// preconditions would defeat its own purpose); the PR body records one real
/// run with the var unset instead.
#[test]
#[ignore = "needs sibling Agent24 + Sin90 checkouts; run explicitly: AGENT24_CHECKOUT=... SIN90_CHECKOUT=... cargo test --features test-hooks --test agent24_mount_blackbox -- --ignored --test-threads=1"]
fn cos72_and_sin90_coexist_isolated() {
    let checkout = agent24_checkout().unwrap_or_else(|| {
        panic!(
            "no Agent24 checkout found (set AGENT24_CHECKOUT or place it at ../Agent24) — this \
             test must FAIL, not silently skip, when its prerequisite is missing"
        )
    });
    let sin90_checkout = sin90_checkout();
    let agent24d_bin = build_agent24d(&checkout);
    let cos72_bin = PathBuf::from(env!("CARGO_BIN_EXE_cos72"));
    let sin90_bin = build_sin90_with_test_hooks(&sin90_checkout);

    let home = tmp_home("coexist-isolated");
    install_cos72(&home.join(".agent24/packages"), &cos72_bin);
    install_sin90(&home.join(".agent24/packages"), &sin90_checkout, &sin90_bin);

    let daemon = start_daemon(&home, &agent24d_bin);
    wait_for_cos72_ready(&daemon, Duration::from_secs(30));
    wait_for_sin90_ready(&daemon, Duration::from_secs(30));

    // ---- both mounted ----
    assert_eq!(
        os_list_entry(&daemon, "cos72")["state"],
        "mounted",
        "daemon log:\n{}",
        daemon.combined_log()
    );
    assert_eq!(
        os_list_entry(&daemon, "sin90")["state"],
        "mounted",
        "daemon log:\n{}",
        daemon.combined_log()
    );

    // ---- Cos72 structurally never holds `scheduler` (T1.1.1's own bar,
    // re-checked here because it is the load-bearing fact behind the
    // owner_module=cos72 == 0 assertion at the end of this test) ----
    let (status, body) =
        http_get(daemon.port, Some(&daemon.token), "/api/v1/cos72/health").unwrap();
    assert_eq!(status, 200, "{body}");
    let health: serde_json::Value = serde_json::from_str(&body).unwrap();
    let caps: Vec<&str> = health["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    assert!(
        !caps.iter().any(|c| c.starts_with("_a24/scheduler/")),
        "cos72 must not hold the scheduler capability: {caps:?}"
    );

    // ---- Cos72 produces (and can recall) its OWN memory marker ----
    let cos72_dedup_key = cos72_complete_one_task(&daemon, "isolation");

    // ---- Sin90 produces (and can recall) its OWN memory marker: a real,
    // finalized Review (mirrors Sin90's own t441_finalized_review_summary_
    // is_recallable_from_kernel_memory) ----
    let human_key = daemon.read_actor_key(&home, "human", Duration::from_secs(10));
    let (status, body) = http_call(
        daemon.port,
        "POST",
        "/api/v1/sin90/reviews",
        Some(&daemon.token),
        Some(&human_key),
        Some(r#"{"kind":"daily","period":"2026-09-30"}"#),
    )
    .unwrap();
    assert_eq!(status, 201, "POST /reviews: {body}");
    let review: serde_json::Value = serde_json::from_str(&body).unwrap();
    let review_id = review["id"].as_str().unwrap().to_owned();

    let (status, body) = http_call(
        daemon.port,
        "PATCH",
        &format!("/api/v1/sin90/reviews/{review_id}"),
        Some(&daemon.token),
        Some(&human_key),
        Some(r#"{"body":"T1.4.1 coexist isolation marker"}"#),
    )
    .unwrap();
    assert_eq!(status, 200, "PATCH /reviews/{{id}}: {body}");

    let (status, body) = http_call(
        daemon.port,
        "POST",
        &format!("/api/v1/sin90/reviews/{review_id}/finalize"),
        Some(&daemon.token),
        Some(&human_key),
        Some("{}"),
    )
    .unwrap();
    assert_eq!(status, 200, "POST /reviews/{{id}}/finalize: {body}");
    let sin90_dedup_key = format!("review:{review_id}");

    // Sin90's own debug route: its `memory_recall_query` param drives an
    // EXTRA `_a24/memory/private/recall` call on Sin90's behalf, alongside
    // that same call's OWN fixed remember/recall probe (kind
    // `t3.2.3.debug`, body `{"probe":"t3.2.3-kernel-clients-roundtrip"}`) —
    // this test's `M_s`, written by every call this closure makes.
    const SIN90_DEBUG_PROBE_MARKER: &str = "t3.2.3-kernel-clients-roundtrip";
    let sin90_recall_extra = |query: &str| -> Vec<serde_json::Value> {
        let (status, body) = http_call(
            daemon.port,
            "POST",
            "/api/v1/sin90/debug/kernel-roundtrip",
            Some(&daemon.token),
            Some(&human_key),
            Some(&serde_json::json!({"memory_recall_query": query}).to_string()),
        )
        .unwrap_or_else(|| {
            panic!(
                "no response from sin90 debug/kernel-roundtrip; daemon log:\n{}",
                daemon.combined_log()
            )
        });
        assert_eq!(
            status,
            200,
            "POST /debug/kernel-roundtrip: {body}; daemon log:\n{}",
            daemon.combined_log()
        );
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        value["memory"]["recall_extra"]["items"]
            .as_array()
            .unwrap_or_else(|| panic!("no memory.recall_extra.items in response: {value}"))
            .clone()
    };

    // 正对照: Sin90 finds its OWN review's dedup_key (the real reconciler
    // pump lands it asynchronously — poll, same reasoning as t441's own
    // wait loop).
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let items = sin90_recall_extra(&sin90_dedup_key);
        if items
            .iter()
            .any(|it| it["body"]["dedup_key"] == sin90_dedup_key)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "sin90 never landed its own review memory for {sin90_dedup_key}; daemon log:\n{}",
            daemon.combined_log()
        );
        std::thread::sleep(Duration::from_millis(500));
    }

    // ---- cross-module memory isolation ----
    // Cos72 queries for Sin90's own fixed debug-route marker (M_s) -> 0.
    let (status, body) = http_post(
        daemon.port,
        Some(&daemon.token),
        "/api/v1/cos72/debug/memory-recall",
        &serde_json::json!({"query": SIN90_DEBUG_PROBE_MARKER}),
    )
    .unwrap();
    assert_eq!(status, 200, "{body}");
    let cross_from_cos72: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        cross_from_cos72["items"].as_array().unwrap().len(),
        0,
        "cos72 must not see sin90's private memory (M_s={SIN90_DEBUG_PROBE_MARKER:?}): \
         {cross_from_cos72}"
    );

    // Sin90 queries for Cos72's own completion dedup_key -> 0 matches.
    let cross_from_sin90 = sin90_recall_extra(&cos72_dedup_key);
    let cross_from_sin90_matches = cross_from_sin90
        .iter()
        .filter(|it| it["body"]["dedup_key"] == cos72_dedup_key)
        .count();
    assert_eq!(
        cross_from_sin90_matches, 0,
        "sin90 must not see cos72's private memory (dedup_key={cos72_dedup_key:?}): \
         {cross_from_sin90:?}"
    );

    // ---- schedule isolation ----
    let _routine = create_routine(
        &daemon,
        &human_key,
        r#"{"title":"coexist isolation routine","kind":"exercise","cron":"0 7 * * MON,WED,FRI","target_count":3}"#,
    );
    // 正对照: at least one owner_module=sin90 row appears (async, via
    // Sin90's own outbox/reconciler — same race routine_m3_real_mount_
    // acceptance's own `wait_for_one_schedule_row` retries around).
    let sin90_row = wait_for_any_schedule_row_owned_by(&daemon, "sin90", Duration::from_secs(20));
    assert_eq!(sin90_row["owner"]["module"], "sin90", "{sin90_row}");

    let schedules = kernel_schedules(&daemon);
    let cos72_rows = schedules["schedules"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["owner"]["module"] == "cos72")
        .count();
    assert_eq!(
        cos72_rows, 0,
        "cos72 must own zero schedule rows (it never requests scheduler): {schedules}"
    );
}
