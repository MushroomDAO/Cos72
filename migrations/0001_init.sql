-- Cos72 domain schema v1 (docs/agent/spec.md「数据模型」), its OWN database
-- (cos72.db under A24_DATA_DIR), physically isolated from the kernel's own
-- agent24.db and from any other module's data dir.
--
-- Connection parameters (set by the pool, not here): journal_mode=WAL,
-- foreign_keys=ON, busy_timeout=5000; every write transaction BEGIN
-- IMMEDIATE (docs/agent/spec.md「数据模型」开头).

CREATE TABLE tasks (
    task_id       TEXT PRIMARY KEY,
    title         TEXT NOT NULL,
    description   TEXT NOT NULL DEFAULT '',
    reward_points INTEGER NOT NULL CHECK (reward_points BETWEEN 1 AND 1000000),
    publisher     TEXT NOT NULL,
    claimer       TEXT NULL,
    status        TEXT NOT NULL CHECK (status IN ('open', 'claimed', 'submitted', 'completed')),
    evidence      TEXT NULL,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    -- docs/agent/spec.md: "open 状态与 claimer 恰好互斥" —— open 时 claimer 必须为
    -- NULL，其余状态 claimer 必须已知。
    CHECK ((status = 'open') = (claimer IS NULL))
);
CREATE INDEX idx_tasks_status_created ON tasks(status, created_at);

CREATE TABLE awards (
    award_id     TEXT PRIMARY KEY,
    task_id      TEXT NOT NULL REFERENCES tasks(task_id),
    member       TEXT NOT NULL,
    points       INTEGER NOT NULL CHECK (points > 0),
    approval_id  TEXT NULL,
    state        TEXT NOT NULL CHECK (state IN ('awaiting', 'credited', 'denied', 'expired')),
    created_at   TEXT NOT NULL,
    decided_at   TEXT NULL,
    last_poll_error TEXT NULL
);
-- 一个任务同时只有一笔在审（docs/agent/spec.md「数据模型」awards 行）。
CREATE UNIQUE INDEX idx_awards_one_awaiting_per_task
    ON awards(task_id) WHERE state = 'awaiting';
CREATE UNIQUE INDEX idx_awards_approval_id
    ON awards(approval_id) WHERE approval_id IS NOT NULL;

-- 只追加的积分账本：余额 = SELECT member, SUM(delta) FROM points_ledger GROUP BY
-- member（回放）。没有余额表/余额列。
CREATE TABLE points_ledger (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    award_id    TEXT NOT NULL UNIQUE REFERENCES awards(award_id),
    member      TEXT NOT NULL,
    delta       INTEGER NOT NULL CHECK (delta > 0),
    task_id     TEXT NOT NULL,
    approval_id TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

-- 账本只追加：UPDATE/DELETE 在数据库层直接失败（docs/agent/architecture.md 不可动摇
-- 的边界 #5）。tests/migrations.rs 的 ledger_rejects_update/ledger_rejects_delete
-- 钉住这两条触发器。
CREATE TRIGGER trg_points_ledger_no_update
BEFORE UPDATE ON points_ledger
BEGIN
    SELECT RAISE(ABORT, 'points_ledger is append-only');
END;

CREATE TRIGGER trg_points_ledger_no_delete
BEFORE DELETE ON points_ledger
BEGIN
    SELECT RAISE(ABORT, 'points_ledger is append-only');
END;

-- Cos72 自己的内核私有记忆出站箱：泵（memory_pump，T1.3.1）按 (state,
-- next_attempt_at) 取，逐行 remember_once。T1.1.1 只落表，不起泵。
CREATE TABLE outbox (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    kind            TEXT NOT NULL CHECK (kind IN ('memory.remember')),
    dedup_key       TEXT NOT NULL UNIQUE,
    payload         TEXT NOT NULL,
    state           TEXT NOT NULL CHECK (state IN ('pending', 'done', 'dead')),
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT NOT NULL,
    last_error      TEXT NULL,
    result_ref      TEXT NULL
);
CREATE INDEX idx_outbox_state_next_attempt ON outbox(state, next_attempt_at);
