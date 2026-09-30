# Cos72 规格 — 落地细节（ME4-5.3 原型）

> 「建成什么样」。精确到能照着建表/实现。架构与边界见 [`architecture.md`](architecture.md)。
> 本文 = ME4-5.3.2–5.3.4 的**设计冻结输入**：实现只能更严，不能更松；要改先改本文与 tasks.md（PLAN-ME4 §一 第 2 条）。
> 带 ⚖️ 的数值是原型阶段的保守默认，可在实现 PR 里论证调整。记录日期：2026-09-29

## 产品定义

Cos72 是跑在 Agent24 内核上的**社区任务与积分**领域 OS（进程外、Rust、用 `agent24-os-sdk` 写成）。核心用例只有一条：
操作者发布带积分的任务 → 成员领取 → 成员提交 → 内核人工审批 → 批准后 Cos72 自己入账，余额由追加式账本回放得出，
完成摘要作为派生副本写进 Cos72 的内核私有记忆，每个动作发事件。

## manifest（`domain-os.yml`，仓库根，打包时按字节原样拷贝）

```yaml
name: cos72
version: "0.1.0"
route_namespace: /api/v1/cos72
event_module: cos72
data_dir: ~/.agent24/os/cos72/
requires_models: []
requires_apis: []
requires_deps: []
kernel_capabilities: [events, memory, approval]
impl_kind: out_of_process_provider
spawn:
  command: bin/cos72
  args: ["module"]
```

- `route_namespace` / `event_module` / `data_dir` 必须与 `name` 派生值逐字相等（内核 `agent24-domain` 安装时校验）。
- 不写 `model_access`（不申请 models）。二进制用 `include_str!("../domain-os.yml")` 把**同一份字节**交给 `Module::builder`。

## 标识与校验

| 值 | 格式 | 说明 |
|:---|:---|:---|
| `task_id` | `tsk_<ULID>` | 服务端生成 |
| `award_id` | `awd_<ULID>` | 服务端生成，一次「申请发积分」一个 |
| `member` / `publisher` | `^[a-z0-9][a-z0-9_-]{0,63}$` | 请求体给出，原型阶段信任操作者（开放问题 Q1） |
| `title` | 1..=200 字符（去首尾空白后非空） | |
| `description` / `evidence` | 0..=4000 字符 | |
| `reward_points` | 整数 1..=1,000,000 ⚖️ | |
| 时间 | RFC 3339 UTC，毫秒精度 | |

## 数据模型（`migrations/0001_init.sql`，ME4-5.3.2 一次落全）

连接参数：`journal_mode=WAL`、`foreign_keys=ON`、`busy_timeout=5000`；所有写事务 `BEGIN IMMEDIATE`。

| 表 | 字段 | 说明 |
|:---|:---|:---|
| `tasks` | `task_id TEXT PK`；`title TEXT NOT NULL`；`description TEXT NOT NULL DEFAULT ''`；`reward_points INTEGER NOT NULL CHECK(reward_points BETWEEN 1 AND 1000000)`；`publisher TEXT NOT NULL`；`claimer TEXT NULL`；`status TEXT NOT NULL CHECK(status IN ('open','claimed','submitted','completed'))`；`evidence TEXT NULL`；`created_at TEXT NOT NULL`；`updated_at TEXT NOT NULL` | `CHECK((status='open') = (claimer IS NULL))`；索引 `(status, created_at)` |
| `awards` | `award_id TEXT PK`；`task_id TEXT NOT NULL REFERENCES tasks`；`member TEXT NOT NULL`；`points INTEGER NOT NULL CHECK(points > 0)`；`approval_id TEXT NULL`；`state TEXT NOT NULL CHECK(state IN ('awaiting','credited','denied','expired'))`；`created_at TEXT NOT NULL`；`decided_at TEXT NULL`；`last_poll_error TEXT NULL` | 部分唯一索引 `UNIQUE(task_id) WHERE state='awaiting'`（一个任务同时只有一笔在审）；`UNIQUE(approval_id) WHERE approval_id IS NOT NULL` |
| `points_ledger` | `seq INTEGER PK AUTOINCREMENT`；`award_id TEXT NOT NULL UNIQUE REFERENCES awards`；`member TEXT NOT NULL`；`delta INTEGER NOT NULL CHECK(delta > 0)`；`task_id TEXT NOT NULL`；`approval_id TEXT NOT NULL`；`created_at TEXT NOT NULL` | **只追加**：`BEFORE UPDATE` / `BEFORE DELETE` 触发器 `RAISE(ABORT, 'points_ledger is append-only')`。原型只有入账（`delta > 0`），扣减留给 myshop |
| `outbox` | `id INTEGER PK AUTOINCREMENT`；`kind TEXT NOT NULL CHECK(kind IN ('memory.remember'))`；`dedup_key TEXT NOT NULL UNIQUE`；`payload TEXT NOT NULL`（JSON）；`state TEXT NOT NULL CHECK(state IN ('pending','done','dead'))`；`attempts INTEGER NOT NULL DEFAULT 0`；`next_attempt_at TEXT NOT NULL`；`last_error TEXT NULL`；`result_ref TEXT NULL`（内核记忆 id） | 泵按 `(state, next_attempt_at)` 取；索引同名 |

**没有**余额表/余额列。余额 = `SELECT member, SUM(delta) FROM points_ledger GROUP BY member`（回放）。

## 状态机

### 任务 `tasks.status`

```
open ──claim(member)──► claimed ──submit(claimer)──► submitted ──award approved──► completed
                           ▲                            │
                           └──────award denied──────────┘
                                                        │ award expired（内核 TTL 300s 到期）
                                                        └──► 仍是 submitted；领取人再 submit → 新 award
```

| 转移 | 触发 | 前置 | 失败码 |
|:---|:---|:---|:---|
| → `open` | `POST /tasks` | 字段合法 | 400 `invalid_request` |
| `open → claimed` | `POST /tasks/{id}/claim {member}` | `status='open'` | 404 `not_found`；409 `invalid_transition` |
| `claimed → submitted` | `POST /tasks/{id}/submit {member, evidence?}` | `member = claimer`；approval 能力在、请求头带 request id + token | 403 `not_claimer`；409；503 `approval_unavailable` |
| `submitted → completed` | award_poller 看到 `approved` | 见「入账」 | — |
| `submitted → claimed` | award_poller 看到 `denied` | | — |
| `completed` 为终态 | 再 claim/submit | | 409 `invalid_transition` |

### 奖励 `awards.state`

```
awaiting(approval_id NULL) ──advise 成功 + CAS 写回──► awaiting(approval_id=X)
awaiting(X) ──status=approved──► credited      （同事务写账本行 + 任务 completed + outbox 行）
awaiting(X) ──status=denied────► denied        （同事务任务回 claimed）
awaiting(X) ──status=timed_out─► expired       （任务保持 submitted）
```

- `awaiting` 且 `approval_id IS NULL` 的行 = 「已落本地、advise 未确认」。poller **不碰**它（没有 id 可查）。
- `credited / denied / expired` 为终态，不再变化。

### submit 的精确步骤（ME4-S3 §2.10 的落地）

1. 取 `RequestContext`；`module.approval()` 为 `None`、或 `request_id`/`approval_token` 缺失 → 503 `approval_unavailable`，**不写库**。
2. `BEGIN IMMEDIATE`：
   - `status='claimed'` 且 `member=claimer` → 改 `submitted`、写 `evidence`；插入 `awards(awd_new, awaiting, approval_id NULL)`。
   - `status='submitted'`：若存在 `awaiting` 行且 `approval_id IS NULL` → **复用该 `award_id`**（孤儿补偿，§2.10 第 4 条第 2 点）；
     若存在 `awaiting` 行且 `approval_id` 非空 → 不再 advise，直接 200 返回现状（幂等）；若不存在 `awaiting` 行（上一笔 `expired`）→ 插入新 `awards` 行。
   - 其它状态 → 409。
   - `COMMIT`。
3. `advise(ApprovalSubmit { action: "cos72.award_points", target: Some(task_id), payload: {award_id, task_id, member, points, title}, request_id, approval_token })`。
   - `Timeout` → 用**同一对** `{request_id, token}` 重试 1 次（wire 幂等，返回同一 `approval_id`）。
   - `ConnectionLost` / `NotSent` → 不重试，回 503（这一代即将 `exit(70)`）。
   - 其它错误 → 回 502 `kernel_error`（带闭集 kind），本地保持 `approval_id IS NULL`，领取人可重新 submit。
4. CAS：`UPDATE awards SET approval_id=? WHERE award_id=? AND state='awaiting' AND approval_id IS NULL`。
   影响 0 行（并发的另一个 submit 先写回了）→ 读回现有 `approval_id` 返回；本次 advise 产生的审批成为孤儿（记 `warn!`，不会入账）。
5. 发事件 `task.submitted`（仅当第 2 步发生了转移）与 `award.requested`；回 202 `{task, award: {award_id, approval_id, state}}`。

### 入账（award_poller，单任务，间隔 3 秒 ⚖️）

对每个 `state='awaiting' AND approval_id IS NOT NULL` 的行调 `status(approval_id)`：
- `pending` → 不动。
- `approved` → `BEGIN IMMEDIATE`：再次确认该行仍 `awaiting`；`INSERT INTO points_ledger(award_id, …)`（`award_id UNIQUE` 冲突 = 已入账，视为成功、不报错）；
  `awards.state='credited'`；`tasks.status='completed'`；插入 outbox `memory.remember`（`dedup_key = cos72:task:<task_id>:completed`，`INSERT OR IGNORE`）；`COMMIT`；
  之后发事件 `award.credited`、`task.completed`。
- `denied` → 同事务 `awards.state='denied'`、`tasks.status='claimed'`；事件 `award.denied`。
- `timed_out` → `awards.state='expired'`；事件 `award.expired`。
- 错误：`NotFound` → 视同 `expired`（内核已无此审批）并记 `last_poll_error`；可重试类 → 记 `last_poll_error`，下轮再查；`ConnectionLost` → 退出循环（进程随后 `exit(70)`）。
- 新一代启动后直接从库里的 `awaiting` 行续查，不依赖内存状态。

### 记忆泵（memory_pump，单任务）

- 取 `state='pending' AND next_attempt_at <= now` 的 outbox 行，逐行 `remember_once(kind="task.summary", dedup_key, body, None)`；body 含
  `task_id, title, member, points, award_id, approval_id, completed_at` 与 `dedup_key`（SDK 自动写入 `body.dedup_key`）。
- `Created{id}` / `Found{id}` → `done`，`result_ref=id`。`Inconclusive` → 退避重试（不当作不存在）。
- 错误分类用**无通配的穷举 match**（ME4-S3 §2.6 / FU-87：新增 `ClientError` 变体时编译失败）：`is_permanent()` → `dead` + `last_error`；
  可重试 → 退避 1s×2^n，上限 5min ⚖️；`ConnectionLost` → 泵停止。
- 没有 memory 能力（`module.memory()` 为 `None`）→ 泵不启动，outbox 行保持 `pending`，不报错。

## REST 路由（相对 `/api/v1/cos72`）

| 方法 路径 | 请求 | 成功 | 说明 |
|:---|:---|:---|:---|
| `GET /health` | — | 200 `{name, version, capabilities}` | `capabilities` = 握手 Offer 实际给到的前缀（便于黑盒核对） |
| `POST /tasks` | `{title, description?, reward_points, publisher}` | 201 task | 事件 `task.published` |
| `GET /tasks?status=` | — | 200 `{tasks: [...]}` | 按 `created_at` 倒序，上限 200 ⚖️ |
| `GET /tasks/{id}` | — | 200 task（含当前 award 摘要） | |
| `POST /tasks/{id}/claim` | `{member}` | 200 task | 事件 `task.claimed` |
| `POST /tasks/{id}/submit` | `{member, evidence?}` | 202 `{task, award}`；已在审且有 id → 200 | 见上 |
| `GET /points` | — | 200 `{balances: [{member, balance}], entries: n}` | 回放求和 |
| `GET /points/{member}` | — | 200 `{member, balance, entries: [...]}` | 无记录 → balance 0 |
| `POST /debug/memory-recall`（仅 `test-hooks`） | `{query}` | 200 `{items: [...]}` | 黑盒核对记忆与隔离用 |

错误体统一 `{"error": {"code": "<snake_case>", "message": "..."}}`；请求体 `deny_unknown_fields`。

## 事件（`_a24/events/emit`，内核盖 `module=cos72`）

`task.published`、`task.claimed`、`task.submitted`、`award.requested`、`award.credited`、`award.denied`、`award.expired`、`task.completed`，
payload 至少含 `task_id`（award 类另含 `award_id`、`approval_id`、`member`、`points`）。启动握手成功后发一次 `module.ready`（黑盒可观测的起点）。

## 错误处理 / 幂等

| 场景 | 处理 | 结果方向 |
|:---|:---|:---|
| advise 已在内核落库、Cos72 没记下 id（写回失败/进程死/客户端取消） | 孤儿留在内核直到 TTL 过期；领取人重交时复用同一 `award_id` 发新 advise | 孤儿被批准也不入账 → 只会少发 |
| 并发两个 submit | 事务串行化；CAS 写回，输家的审批成孤儿 | 最多一笔入账 |
| poller 入账后、发事件前进程死 | 新一代重查：行已 `credited` 不再处理；事件可能丢（事件非真相） | 账正确，事件至多少一条 |
| 账本插入与状态更新 | 同一事务；`award_id UNIQUE` 兜底 | 不重复 |
| `remember_once` 超时后重试 | 预查 `dedup_key`；残余重复与 Sin90 相同（FU-85 根治） | 已接受残余风险 |
| 被批准的孤儿 | 用户文档写明「无效」；审批界面上 payload 的 `award_id` 可识别 | — |

## 测试策略

- **单测（`cargo test`，CI 必跑）**：`core` 状态机穷举转移表；store 仓储（临时文件库）；路由用 `tower::ServiceExt::oneshot` + 假 `KernelPort`；
  poller/pump 用假端口驱动 approved/denied/timed_out/NotFound/Inconclusive/各错误类；wire 对等用 `agent24_os_sdk::testing::fake_kernel` 断言方法名与参数字段。
- **迁移测试**：约束的正/负样本（重复 `award_id`、同任务两笔 `awaiting`、UPDATE/DELETE 账本 → 失败；合法插入 → 成功）。
- **结构测试**：源码不含 socket 类型与协议字节解析；源码不含 `UPDATE points_ledger` / `DELETE FROM points_ledger`；`remember_once` 只在 `workers/memory_pump.rs` 出现；`.advise(` 只在 submit handler 出现。
- **真实挂载黑盒（`#[ignore]`，本地必跑）**：`tests/agent24_mount_blackbox.rs`，需要 `AGENT24_CHECKOUT`（及 5.3.4 起 `SIN90_CHECKOUT`）；前置缺失即**失败**，不跳过。
- 全部新回归测试做变异验证（改回去必须变红），PR body 写明；`cargo test <过滤>` 类验收先 `-- --list` 断言匹配数 > 0。
