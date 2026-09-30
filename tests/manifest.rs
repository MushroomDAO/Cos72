//! docs/agent/tasks.md T1.1.1 验收命令 #1: `domain-os.yml`'s own fields, and
//! the binary's embedded copy of it, each checked against what the frozen
//! design (docs/agent/spec.md「manifest」) actually requires — not against
//! each other, so a bug that changes both the file and this test in the
//! same wrong way still gets caught.

use serde::Deserialize;

#[derive(Deserialize)]
struct RawManifest {
    name: String,
    route_namespace: String,
    event_module: String,
    data_dir: String,
    kernel_capabilities: Vec<String>,
}

fn manifest_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("domain-os.yml")
}

fn parse() -> RawManifest {
    let text = std::fs::read_to_string(manifest_path()).expect("domain-os.yml must exist");
    serde_yaml::from_str(&text).expect("domain-os.yml must be valid YAML matching RawManifest")
}

/// `route_namespace` / `event_module` / `data_dir` must derive from `name`
/// (docs/agent/spec.md「manifest」: "必须与 name 派生值逐字相等（内核
/// agent24-domain 安装时校验）").
///
/// 正对照 (docs/agent/tasks.md T1.1.1 验收命令 #1): temporarily change
/// `route_namespace` in `domain-os.yml` to `/api/v1/cos` — this test goes
/// red (verified by hand for this PR; see PR body for the exact diff/output).
#[test]
fn manifest_fields_derive_from_name() {
    let m = parse();
    assert_eq!(m.route_namespace, format!("/api/v1/{}", m.name));
    assert_eq!(m.event_module, m.name);
    assert_eq!(m.data_dir, format!("~/.agent24/os/{}/", m.name));
}

/// docs/agent/architecture.md 核心判断 3: exactly `events`, `memory`,
/// `approval` — no `scheduler`, no `models`.
///
/// 正对照 (docs/agent/tasks.md T1.1.1 验收命令 #1): temporarily add
/// `scheduler` to `domain-os.yml`'s `kernel_capabilities` — this test goes
/// red (verified by hand for this PR; see PR body).
#[test]
fn manifest_capabilities_are_exactly_events_memory_approval() {
    let m = parse();
    assert_eq!(m.kernel_capabilities, vec!["events", "memory", "approval"]);
}

/// `cos72::MANIFEST` (`include_str!("../domain-os.yml")`, compiled into the
/// binary and sent to the kernel at `initialize`) must be the exact same
/// bytes as the file on disk this test just read at runtime — guards
/// against a stale build or a second, independent embed silently drifting
/// from the file the kernel's `manifest_digest` check compares against
/// (docs/agent/architecture.md「契约 / 接口」"manifest 字节与二进制里
/// include_str! 的是同一份").
#[test]
fn binary_embeds_the_same_manifest_bytes() {
    let on_disk = std::fs::read_to_string(manifest_path()).unwrap();
    assert_eq!(cos72::MANIFEST, on_disk);
}
