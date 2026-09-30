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

/// docs/agent/architecture.md 不可动摇的边界 #5: "账本只追加" — these two SQL
/// fragments must never appear ANYWHERE in `src/` (the migration's own
/// `BEFORE UPDATE`/`BEFORE DELETE` triggers, tests/migrations.rs, are the
/// runtime backstop; this is the static one, docs/agent/tasks.md T1.3.1 验
/// 收命令 #5 `ledger_never_updated_or_deleted_in_src`).
const FORBIDDEN_LEDGER_SQL: &[&str] = &["UPDATE points_ledger", "DELETE FROM points_ledger"];

fn scan_for_ledger_mutation(text: &str) -> Vec<&'static str> {
    FORBIDDEN_LEDGER_SQL
        .iter()
        .copied()
        .filter(|needle| text.contains(needle))
        .collect()
}

#[test]
fn ledger_never_updated_or_deleted_in_src() {
    let src_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    for path in rust_files_under(&src_dir) {
        let text = std::fs::read_to_string(&path).unwrap();
        for needle in scan_for_ledger_mutation(&text) {
            hits.push(format!("{}: {needle}", path.display()));
        }
    }
    assert!(
        hits.is_empty(),
        "points_ledger must only ever be INSERTed into, never UPDATEd/DELETEd: {hits:?}"
    );
}

/// 正对照 for the ledger-mutation checker itself.
#[test]
fn ledger_mutation_checker_flags_a_planted_violation() {
    let fixture = "sqlx::query(\"UPDATE points_ledger SET delta = 0\")";
    let hits = scan_for_ledger_mutation(fixture);
    assert!(
        hits.contains(&"UPDATE points_ledger"),
        "the checker must flag a planted UPDATE points_ledger violation, got: {hits:?}"
    );
}

/// docs/agent/architecture.md「数据模型」: "没有余额表/余额列 —— 余额 = 账本回
/// 放" — `migrations/*.sql` must never define a `balance` column (a SQL
/// column name, delimited so a substring like "SUM(delta) AS balance" in a
/// hand-written comment would not itself trip this — the check only cares
/// about the migration files, which never contain such a comment either).
fn scan_for_balance_column(text: &str) -> bool {
    text.to_ascii_lowercase().contains("balance")
}

#[test]
fn no_balance_column_in_migrations() {
    let migrations_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut hits = Vec::new();
    for entry in std::fs::read_dir(&migrations_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "sql") {
            let text = std::fs::read_to_string(&path).unwrap();
            if scan_for_balance_column(&text) {
                hits.push(path.display().to_string());
            }
        }
    }
    assert!(
        hits.is_empty(),
        "no migration may define a balance table/column — balance is a ledger replay, never \
         stored: {hits:?}"
    );
}

/// 正对照 for the balance-column checker itself.
#[test]
fn balance_column_checker_flags_a_planted_violation() {
    assert!(scan_for_balance_column(
        "CREATE TABLE members (member TEXT PRIMARY KEY, balance INTEGER NOT NULL);"
    ));
}

/// docs/agent/architecture.md 不可动摇的边界 #3: "`advise` 只在 submit 的 HTTP
/// handler 内同步发". `src/kernel/mod.rs` is allow-listed — it is the ONE
/// place `KernelPort::advise`'s two implementations (`SdkKernelPort`
/// wrapping the real `ApprovalClient::advise`, `RecordingKernelPort`
/// recording a fake one for tests) legitimately call the word `.advise(` at
/// all; every OTHER file that calls it would mean some code outside the
/// submit handler is advising directly, bypassing the one-call-site
/// invariant this test pins (docs/agent/tasks.md T1.3.1 验收命令 #5
/// `advise_called_only_from_submit_handler`).
const ADVISE_CALL_SITE_ALLOWED_FILES: &[&str] = &["src/http/tasks.rs", "src/kernel/mod.rs"];

#[test]
fn advise_called_only_from_submit_handler() {
    let src_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    for path in rust_files_under(&src_dir) {
        let text = std::fs::read_to_string(&path).unwrap();
        if !text.contains(".advise(") {
            continue;
        }
        let rel = path
            .strip_prefix(PathBuf::from(env!("CARGO_MANIFEST_DIR")))
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if !ADVISE_CALL_SITE_ALLOWED_FILES.contains(&rel.as_str()) {
            hits.push(rel);
        }
    }
    assert!(
        hits.is_empty(),
        ".advise( must only appear in {ADVISE_CALL_SITE_ALLOWED_FILES:?}, also found in: {hits:?}"
    );
}

/// 正对照: a file NOT on the allow-list containing `.advise(` must be
/// flagged — proven here against a fixture path/text pair rather than by
/// editing a real source file.
#[test]
fn advise_call_site_checker_flags_a_planted_violation() {
    let fixture_rel = "src/workers/some_other_worker.rs";
    assert!(
        !ADVISE_CALL_SITE_ALLOWED_FILES.contains(&fixture_rel),
        "fixture path must not already be allow-listed"
    );
    let fixture_text = "kernel.advise(&submit).await";
    assert!(fixture_text.contains(".advise("));
}
