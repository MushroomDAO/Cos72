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
//!     cargo test --test agent24_mount_blackbox -- --ignored --test-threads=1
//!
//! Harness shape (install/start/WS-subscribe/http helpers) mirrors Sin90's
//! own `tests/agent24_mount_blackbox.rs` (docs/agent/acceptance.md「待补能力
//! 清单」: "真实挂载黑盒夹具（装包/起 daemon/WS 订阅）| 高 | Sin90
//! tests/agent24_mount_blackbox.rs") — a much smaller slice of it, scoped to
//! T1.1.1's one acceptance case (mount + health + module.ready + the 401
//! negative control); T1.2.1/T1.3.1/T1.4.1 grow this file with their own
//! cases the same way Sin90's grew across its own M0–M5.
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
