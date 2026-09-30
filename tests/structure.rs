//! docs/agent/tasks.md T1.1.1 验收命令 #4 / docs/agent/architecture.md 不可
//! 动摇的边界 #7: "Cos72 从不碰 socket、从不自己解析协议帧" — a structural
//! grep over `src/`, not a convention anyone has to remember. The positive
//! control (`checker_flags_a_planted_violation`) proves the checker itself
//! actually detects a violation, rather than vacuously passing because it
//! never matches anything.

use std::path::PathBuf;

/// Identifiers that mean "this code is talking to a raw socket/fd or hand-
/// parsing a protocol frame itself" — all of that lives in `agent24-os-sdk`
/// / `agent24-os-proto` now, never in Cos72 (docs/agent/architecture.md 核
/// 心判断 2: "只依赖 SDK，不直接依赖 agent24-os-proto").
const FORBIDDEN: &[&str] = &[
    "UnixStream",
    "UnixListener",
    "TcpListener",
    "from_raw_fd",
    "OwnedFd",
    "serde_json::from_slice",
];

fn scan_for_forbidden_identifiers(text: &str) -> Vec<&'static str> {
    FORBIDDEN
        .iter()
        .copied()
        .filter(|needle| text.contains(needle))
        .collect()
}

fn rust_files_under(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap_or_else(|e| panic!("read_dir {d:?}: {e}")) {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out
}

/// docs/agent/tasks.md T1.1.1 验收命令 #4.
#[test]
fn no_socket_or_frame_parsing_in_src() {
    let src_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    for path in rust_files_under(&src_dir) {
        let text = std::fs::read_to_string(&path).unwrap();
        for needle in scan_for_forbidden_identifiers(&text) {
            hits.push(format!("{}: {needle}", path.display()));
        }
    }
    assert!(
        hits.is_empty(),
        "forbidden socket/frame-parsing identifiers found in src/: {hits:?}"
    );
}

/// 正对照: the checker function, run on a fixture string that DOES contain a
/// planted violation, must report it — proves
/// `no_socket_or_frame_parsing_in_src` would actually fail if `src/` ever
/// grew one of these identifiers, rather than passing vacuously.
#[test]
fn checker_flags_a_planted_violation() {
    let fixture = "use std::os::unix::net::UnixStream;\nfn open() -> UnixStream { todo!() }\n";
    let hits = scan_for_forbidden_identifiers(fixture);
    assert!(
        hits.contains(&"UnixStream"),
        "the checker must flag a planted UnixStream violation, got: {hits:?}"
    );
}
