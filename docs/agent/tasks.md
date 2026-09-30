# Cos72 任务台账 — Task

> 前置：[`roadmap.md`](roadmap.md)（M→F）·[`architecture.md`](architecture.md)·[`spec.md`](spec.md)（设计冻结输入）·[`acceptance.md`](acceptance.md)
> **本文件是 Cos72 唯一的执行状态来源**。跨仓库的门在 Agent24 `docs/agent/tasks.md`「ME-4 台账」（ME4-5.3.x 行），完整任务定义以本文件为准。
> 状态：BACKLOG · READY · IN_PROGRESS · BLOCKED · PR_OPEN · CHANGES_REQUESTED · APPROVED · DONE
>
> **全局验收前置**（每个 task 都适用，不再逐条重复）：
> ```
> cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test
> ```
> **真实挂载验收**（凡是碰到 manifest / SDK / 与内核交互的 task 都要跑；前置缺失即**失败**，不跳过）：
> ```
> AGENT24_CHECKOUT=$HOME/Dev/auraai/Agent24 SIN90_CHECKOUT=$HOME/Dev/auraai/sin90-design \
>   cargo test --test agent24_mount_blackbox -- --ignored --test-threads=1
> ```
> 跑之前把 Agent24 更新到 main（≥ v0.4.0，SDK CHANGELOG 的最低内核版本）：`git -C "$AGENT24_CHECKOUT" fetch origin && git -C "$AGENT24_CHECKOUT" merge --ff-only origin/main`。
> `SIN90_CHECKOUT` 只有 T1.4.1 起才需要，且必须是已迁到 SDK 的 Sin90 main（ME4-5.2.1 合并后）。
> **验收命令不许空转**：`cargo test <过滤>` 形式的验收，先跑 `cargo test <同参数> -- --list` 并断言匹配数 > 0（cargo 匹配零个测试也返回成功）。
> **新回归测试一律变异验证**：把实现改回去/删掉约束，确认测试变红；PR body 写明变异方式与结果。**每条判据带正对照**（下文「正对照」行）。
> **合并判据**：clestons 的 APPROVED review 的 `commit_id` == PR 当前 `headRefOid`，且 check 全 SUCCESS；main 开 ruleset 前用 `gh pr merge <n> --squash`。approve 之后不再往该分支推 commit。
> **台账回填**：PR 合并后 `DONE`/证据由下一个 PR 顺带回填（Agent24 PLAN-ME4 §一 第 7 条），同时回填 Agent24 ME-4 台账对应行。
> **流程**：一个 task 一个分支一个 PR（`feat/`、`docs/`、`test/` 前缀），base 恒为 `main`，上一片合并后下一片才开（不叠 PR）；
> 自审 → Codex 挑战（`codex:codex-rescue`；Codex 不可用时 Tier-2 全新上下文 Opus 对抗评审 + 记 `ME4-CODEX-DEBT`）→
> `bash ~/Dev/tools/PR-daemon/scripts/pre-pr-check.sh --base main`（PR body 按编号回应命中项）→ PR → clestons。
> **规模**：ME4-S3 §7 v4 允许原型大片超过 SZ-1 默认 300 行，超限在 PR body 说明即可；下面的行数是不含 `Cargo.lock` 的估计。

## 与 Agent24 ME-4 台账的映射（门）

| Cos72 Task | Agent24 ID | 依赖（跨仓库） | 状态 |
|:---|:---|:---|:---|
| T1.0.1 | ME4-5.3.1 | ME4-5.1.2b（tag `agent24-os-sdk-v0.1.0` 已存在） | `IN_PROGRESS` |
| T1.1.1 | ME4-5.3.2 | T1.0.1 | `BACKLOG` |
| T1.2.1 | ME4-5.3.3a | T1.1.1 | `BACKLOG` |
| T1.3.1 | ME4-5.3.3b | T1.2.1 | `BACKLOG` |
| T1.4.1 | ME4-5.3.4 | T1.3.1 + ME4-5.2.1（Sin90 迁到 SDK） | `BACKLOG` |
| T2.1.1 | ME4-6.0.2（Cos72 部分） | T1.4.1 + ME4-6.0.1（发布清单冻结） | `IN_PROGRESS` |

**需 jason 手动做（不是 goal task）**：① 给 `MushroomDAO/Cos72` main 开 ruleset（1 个审批 + dismiss stale）；② 确认 PR-Daemon（clestons）监听 `MushroomDAO/Cos72`（开放问题 Q7）。

---

## F1.0 — 规划层

### T1.0.1 pilot 七件套 + `.pilot.yml`（ME4-5.3.1）  `IN_PROGRESS`
- **优先级**：high
- **目标**：Cos72 有可无人值守执行的规划层；5.3.2–5.3.4 的 task、验收、PR 规模、开放问题全部落在本仓库。
- **开发范围**：`.pilot.yml`、`docs/agent/{research,acceptance,architecture,spec,roadmap,tasks,progress}.md`。
- **明确不做**：任何代码、Cargo 工程；不改 Agent24 仓库（Agent24 ME-4 台账的回填由后续 PR 顺带做）。
- **依赖**：无（SDK tag 已在）
- **交付物**：上述 8 个文件。
- **验收命令**：`bash ~/.claude/skills/pilot/scripts/check-docs.sh --strict` 退出码 0 且输出 `ok=7/7`。
  正对照：临时把 `docs/agent/spec.md` 改名 → 同一命令退出码 1 且列出 `MISSING … spec.md`（已在本分支实测，见 progress.md）。
- **涉及文件**：见交付物。
- **证据**：分支 `docs/pilot-plan`（已推送，未开 PR，等 jason 过目开放问题）。

---

## F1.1 — 骨架

### T1.1.1 Cargo 工程 + manifest + SDK 挂载 + 迁移 + 事件 + CI（ME4-5.3.2）  `BACKLOG`
- **优先级**：high
- **目标**：一个能被真实 agent24d 装上、握手成功、挂载 `/api/v1/cos72/health`、发出 `module.ready` 事件的空壳，库里已有 spec.md 的全量 schema。
- **开发范围**：
  - `Cargo.toml`（edition 2021；`agent24-os-sdk` git+tag 依赖，dev 依赖同 tag 开 `test-util`；axum 0.8、sqlx 0.8 sqlite+migrate、tokio、serde、thiserror、tracing、ulid；feature `test-hooks`）、`Cargo.lock`、`.gitignore` 加 `/target`。
  - `domain-os.yml`（spec.md「manifest」逐字）；`src/main.rs`：`Module::builder(MANIFEST).connect()` → 开库 → `module.serve(router)`；`MANIFEST = include_str!("../domain-os.yml")`。
  - `src/kernel/`：`KernelPort` trait（`emit` / `advise` / `approval_status` / `remember_once`，本 task 只实现 `emit`，其余返回 `Unavailable` 占位并在 T1.3.1 实现）+ SDK 实现 + 测试用记录型假实现。
  - `src/store/` + `migrations/0001_init.sql`：spec.md「数据模型」全部四张表、索引、触发器；WAL/FK/busy_timeout。
  - `src/http/`：`GET /health`（返回 name、version、Offer 实际给到的前缀）。
  - `tests/agent24_mount_blackbox.rs`：装包/起 daemon/WS 订阅夹具（照 Sin90 同名文件）+ 一条挂载冒烟。
  - `.github/workflows/ci.yml`：ubuntu + macos、stable、fmt/clippy/test（黑盒是 `#[ignore]`，不进 CI）。
- **明确不做**：任务/奖励/账本的业务代码与路由；standalone 模式；scheduler/models 能力。
- **依赖**：T1.0.1
- **交付物**：可编译可挂载的 `cos72` 二进制；schema 全量迁移；CI。
- **验收命令**（全局前置之外）：
  1. `cargo test --test manifest`（≥ 3 条：`manifest_fields_derive_from_name`、`manifest_capabilities_are_exactly_events_memory_approval`、`binary_embeds_the_same_manifest_bytes`）。
     正对照：往 `domain-os.yml` 的 `kernel_capabilities` 加 `scheduler` → 第 2 条变红；把 `route_namespace` 改成 `/api/v1/cos` → 第 1 条变红。
  2. `cargo test --test migrations`（≥ 5 条：`ledger_rejects_update`、`ledger_rejects_delete`、`duplicate_award_id_in_ledger_rejected`、`second_awaiting_award_for_same_task_rejected`、`open_task_with_claimer_rejected`），每条在同一测试里先做合法插入（正样本成功）再做违规写（负样本失败）。
     变异：删掉 `BEFORE UPDATE` 触发器 → `ledger_rejects_update` 变红；删部分唯一索引 → 第 4 条变红。
  3. `cargo test kernel::`（≥ 2 条：`handshake_declares_exactly_manifest_capabilities`——用 `testing::FakeEndpoint::accept_initialize` 断言握手 params 的 capabilities 为 `["events","memory","approval"]`、协议范围 `{min:1,max:1}`；`emit_sends_events_emit_with_kind_and_payload`——断言方法名 `_a24/events/emit`）。
  4. `cargo test --test structure no_socket_or_frame_parsing_in_src`：`src/` 不出现 `UnixStream|UnixListener|TcpListener|from_raw_fd|OwnedFd|serde_json::from_slice`。正对照：同文件的 `checker_flags_a_planted_violation` 把检查函数跑在含 `UnixStream` 的夹具字符串上必须报命中。
  5. 真实挂载：`AGENT24_CHECKOUT=… cargo test --test agent24_mount_blackbox cos72_mounts_and_answers_health -- --ignored --test-threads=1`：
     `GET /api/v1/os` 里 cos72 `mounted`；带 daemon token 的 `GET /api/v1/cos72/health` 200 且 capabilities 含 `_a24/events/`、`_a24/memory/private/`、`_a24/approval/`、**不含** `_a24/scheduler/`；WS 收到 `module=cos72` 的 `module.ready`。
     负对照：不带 token 的同一请求 → 401（证明请求确实经过内核而不是直连）。
  6. CI：`gh run list -R MushroomDAO/Cos72 --branch <本分支> --limit 1 --json conclusion -q '.[0].conclusion'` = `success`。
- **涉及文件**：`Cargo.toml`、`Cargo.lock`、`.gitignore`、`domain-os.yml`、`src/{main,lib}.rs`、`src/{kernel,store,http}/`、`migrations/0001_init.sql`、`tests/{manifest,migrations,structure,agent24_mount_blackbox}.rs`、`.github/workflows/ci.yml`。
- **规模估计**：≈ 700–900 行（源码 ~350、迁移 ~80、测试 ~400、CI ~40）。超 300 行，PR body 说明「骨架原子单元：schema 与挂载夹具被后续三片共用」。
- **风险/回滚**：SDK 是 git 依赖，CI 需能拉取公开仓库 `iDoris-ai/Agent24`（公开，已核）；proto 带来的冷编译时间在 PR body 记录（FU-86 的数据点）。
- **证据**：

---

## F1.2 — mytask 实体与路由

### T1.2.1 任务状态机 + 发布/领取/提交/查询路由 + 事件（ME4-5.3.3a）  `BACKLOG`
- **优先级**：high
- **目标**：mytask 从 `open` 走到 `submitted`；提交时**只落本地** `awaiting` 奖励行（`approval_id IS NULL`，即 ME4-S3 §2.10「先落本地」那半步），advise 在 T1.3.1 接上。
- **开发范围**：`src/core/task.rs`（纯状态机 + 校验）、`src/store/tasks.rs`、`src/http/tasks.rs`（spec.md 路由表的 `POST /tasks`、`GET /tasks`、`GET /tasks/{id}`、`POST …/claim`、`POST …/submit`）、事件 `task.published / task.claimed / task.submitted`、统一错误体。
- **明确不做**：advise、轮询、账本、记忆、`/points` 路由（T1.3.1）；任务取消/退领/截止时间。
- **依赖**：T1.1.1
- **交付物**：上述路由与状态机，单测 + 黑盒扩展。
- **验收命令**：
  1. `cargo test core::task::`：`transition_table_is_exhaustive`（4 个状态 × {claim, submit} 全组合，允许的恰为 spec.md 表中两条，其余 `InvalidTransition`）。变异：放开 `completed → claimed` → 变红。
  2. `cargo test http::tasks::`（≥ 7 条）：`publish_returns_201_open`；`publish_points_bounds`（0 与 1,000,001 → 400；1 与 1,000,000 → 201，正对照在同一测试）；`claim_twice_is_409`；`submit_by_non_claimer_is_403`（正对照：领取人提交 → 202）；
     `submit_inserts_exactly_one_awaiting_award_with_null_approval_id`；`concurrent_claims_exactly_one_wins`（10 个并发 claim → 恰 1 个 200、9 个 409）；`unknown_body_fields_rejected`。
  3. `cargo test events::`：`each_transition_emits_one_event_carrying_task_id`（记录型假端口；正对照：失败的 409 请求**不**发事件）。
  4. 真实挂载：`… cargo test --test agent24_mount_blackbox cos72_task_routes_through_real_proxy -- --ignored --test-threads=1`：经代理发布 → 领取 → 提交，`GET /tasks/{id}` 为 `submitted`；WS 依次收到 `task.published`、`task.claimed`、`task.submitted`（`module=cos72`）。
- **涉及文件**：`src/core/`、`src/store/tasks.rs`、`src/http/tasks.rs`、`src/http/error.rs`、`tests/agent24_mount_blackbox.rs`。
- **规模估计**：≈ 800–1000 行（源码 ~400、测试 ~500）。
- **风险/回滚**：无涉钱操作；本片合并后 main 上的 submit 不发审批（中间态，T1.3.1 补齐），不发布。
- **证据**：

---

## F1.3 — 审批发积分 + 账本 + 记忆

### T1.3.1 submit 内 advise + status 轮询入账 + 追加式账本 + outbox 串行 remember_once（ME4-5.3.3b）  `BACKLOG`
- **优先级**：high
- **目标**：批准才入账、每笔奖励至多入账一次、孤儿只会少发；余额 = 账本回放；完成摘要经 outbox 单泵恰好一次进内核记忆。
- **开发范围**：
  - `KernelPort` 的 `advise` / `approval_status` / `remember_once` SDK 实现。
  - submit handler 接上 spec.md「submit 的精确步骤」第 1、3、4、5 步（能力/头检查、advise、`Timeout` 同对重试 1 次、CAS 写回、`award.requested`）。
  - `src/workers/award_poller.rs`（spec.md「入账」）、`src/workers/memory_pump.rs`（spec.md「记忆泵」，无通配穷举 match 分类 `ClientError`）。
  - `src/store/ledger.rs`、`src/http/points.rs`（`GET /points`、`GET /points/{member}`）；`test-hooks` 下 `POST /debug/memory-recall`。
- **明确不做**：审批 UI（用 Agent24 既有 `/api/v1/module-approvals`）；扣积分；内核改动（FU-84/FU-85 不在本轮）。
- **依赖**：T1.2.1
- **交付物**：完整审批往返与账本；memory 派生摘要。
- **验收命令**：
  1. `cargo test approval::`（≥ 6 条，`testing::fake_kernel` 钉 wire）：
     `submit_advises_inside_request_with_its_request_id_and_token`（方法 `_a24/approval/advise`，params 的 `request_id`/`approval_token` 等于请求头，`payload.award_id` 等于库里那行）；
     `submit_without_approval_capability_is_503_and_writes_nothing`（正对照：有能力时 202 且库里多一行）；
     `advise_timeout_retries_once_with_the_same_pair`（假内核第一次回 timeout，第二次同 params 成功；断言恰 2 次调用）；
     `advise_failure_leaves_null_approval_id_and_resubmit_reuses_award_id`；
     `resubmit_while_awaiting_with_id_does_not_advise_again`（advise 调用计数仍为 1）；
     `lost_cas_race_returns_winner_approval_id`。
  2. `cargo test poller::`（≥ 5 条）：`approved_credits_exactly_once`（poller 连跑 3 轮，账本该 `award_id` 恰 1 行）；
     `approved_orphan_is_never_credited`（假内核对一个不在 `awards` 里的 approval_id 回 approved → 账本 0 行；正对照：库里记下的那个 → 1 行）；
     `denied_returns_task_to_claimed_without_ledger`；`timed_out_marks_expired_and_resubmit_creates_new_award`；`not_found_marks_expired`。
     变异：去掉入账事务里「行仍 `awaiting`」的复核 → `approved_credits_exactly_once` 仍被 `award_id UNIQUE` 兜住（PR body 记录两层防线各自删掉时的读数）。
  3. `cargo test ledger::`：`balance_equals_replay_of_ledger`（随机 50 笔入账，`GET /points` 逐人等于 `SUM(delta)`）；`restart_rebuilds_same_balances`（关库重开后相等）。
  4. `cargo test memory::`（≥ 3 条）：`completion_enqueues_exactly_one_outbox_row`；`pump_calls_remember_once_with_namespaced_dedup_key`（`cos72:task:<id>:completed`，方法 `_a24/memory/private/recall` 预查后 `…/remember`）；`inconclusive_is_retried_not_marked_done`。
  5. `cargo test --test structure`：`remember_once_called_only_from_memory_pump`、`advise_called_only_from_submit_handler`、`ledger_never_updated_or_deleted_in_src`、`no_balance_column_in_migrations`，每条有「夹具里植入违规 → 检查函数报命中」的正对照。
  6. 真实挂载：`… cargo test --test agent24_mount_blackbox cos72_award_roundtrip_under_real_daemon -- --ignored --test-threads=1`：
     submit 后 `GET /api/v1/module-approvals?decision=pending` 有 `module=cos72`、`kind=advise`、`payload.award_id` 匹配的一条 → `POST …/{id} {"decision":"approved"}` → 30 秒内任务 `completed`、`GET /points/{member}` = 奖励 → `/debug/memory-recall` 以 dedup_key 查恰 1 条 →
     重启 daemon 后余额不变、记忆仍恰 1 条；另一任务走 `denied` → 回 `claimed`、余额不变（正对照）。
- **涉及文件**：`src/kernel/`、`src/http/{tasks,points,debug}.rs`、`src/workers/`、`src/store/ledger.rs`、`tests/`。
- **规模估计**：≈ 1100–1400 行（源码 ~550、测试 ~700）。若自审时评审负担过大，可按层内部拆成 3b-1（advise + poller + 账本）/ 3b-2（outbox + 记忆），但仍一次只开一个 PR。
- **风险/回滚**：**涉钱**（积分）。入账的三重条件（自记 approval_id、内核 approved、award_id 未入账）任何一条被削弱都是 Critical；已接受残余：孤儿被批准不入账（少发）、`remember_once` 超时后极小概率重复摘要（FU-85）。
- **证据**：

---

## F1.4 — 真实挂载黑盒

### T1.4.1 全流程黑盒 + 与 Sin90 共存隔离（ME4-5.3.4）  `BACKLOG`
- **优先级**：high
- **目标**：只用真实 agent24d + 真实 cos72 + 真实 sin90 二进制，证明「安装 → 挂载 → 全流程 → 审批往返 → 两模块同时挂载时互相读不到对方的记忆与 schedules」。
- **开发范围**：`tests/agent24_mount_blackbox.rs` 增加 `cos72_full_flow_real_mount` 与 `cos72_and_sin90_coexist_isolated`；夹具增加从 `SIN90_CHECKOUT` 以 `test-hooks` 构建 sin90（独立 `--target-dir`）并装包。
- **明确不做**：修改 Sin90 或 Agent24；给 Cos72 加 scheduler 能力（开放问题 Q5）。
- **依赖**：T1.3.1、ME4-5.2.1（Sin90 main 已迁到 SDK）
- **交付物**：两条 `#[ignore]` 黑盒测试；Agent24 ME-4 台账 5.3.x 行的回填（顺带 PR）。
- **验收命令**：
  1. `AGENT24_CHECKOUT=… SIN90_CHECKOUT=… cargo test --test agent24_mount_blackbox -- --ignored --test-threads=1` 全绿（含 T1.1.1–T1.3.1 累积的全部黑盒）；先 `-- --ignored --list` 断言 ≥ 5 条。
  2. `cos72_and_sin90_coexist_isolated` 内的判据：
     - `GET /api/v1/os`：cos72 与 sin90 均 `mounted`。
     - 记忆：经 Sin90 的 test-hooks `kernel-roundtrip` 写入标记 `M_s`；Cos72 `/debug/memory-recall {query: M_s}` → 0 条；Sin90 以 Cos72 的完成摘要 dedup_key 查 → 0 条。**正对照**：各自查自己的标记/摘要 → ≥ 1 条。
     - 调度：经 Sin90 建一个 Routine → `GET /api/v1/schedules` 出现 `owner_module=sin90` 的行（**正对照**）；`owner_module=cos72` 的行数为 0；Cos72 `/health` 的 capabilities 不含 `_a24/scheduler/`。
  3. 前置缺失即失败：不设 `SIN90_CHECKOUT` 时该测试 panic（不是 pass/skip）——PR body 附一次实测输出。
- **涉及文件**：`tests/agent24_mount_blackbox.rs`、`docs/agent/{tasks,progress}.md`。
- **规模估计**：≈ 700–900 行测试代码（夹具复用 T1.1.1 起的实现，Sin90 装包/key 读取约 200 行）。
- **风险/回滚**：依赖两个外部检出的版本；PR body 记录三仓库的 commit SHA。调度隔离从内核 REST 侧验，是 ME4-S3 §8 Q4 明确允许的路径，**不是**模块侧「调了 `_a24/scheduler/list` 看不到」的证明——措辞按此写，不夸大。
- **证据**：

---

## F2.1 — 打包与 Release（M2）

### T2.1.1 可安装包 + SHA256SUMS + GitHub Release（ME4-6.0.2 的 Cos72 部分）  `IN_PROGRESS`
- **优先级**：mid
- **目标**：按 ME4-6.0.1 冻结的清单产出 Cos72 发布物。
- **依赖**：T1.4.1、ME4-6.0.1
- **验收命令**：
  1. `cd scripts/.. && bash scripts/package.sh` 以 exit code 0 结束（脚本自带自检，任何一项不
     符合就非 0 退出，见下）；只在 `uname -sm` 为 `Darwin arm64` 上跑。
  2. `cd dist && shasum -a 256 -c SHA256SUMS` 输出 `cos72-<ver>-macos-arm64.tar.gz: OK`
     （脚本末尾已内置此检查，此处为人工复核）。
  3. `tar -tzf dist/cos72-<ver>-macos-arm64.tar.gz | grep -v '/$'` 只列出两行：
     `cos72-<ver>-macos-arm64/domain-os.yml` 与 `cos72-<ver>-macos-arm64/bin/cos72`
     （脚本末尾已内置此检查）。
  4. `tar -tvzf dist/cos72-<ver>-macos-arm64.tar.gz | grep bin/cos72` 权限位含 `x`
     （二进制可执行）。
  5. GitHub Release 部分（上传 tarball + SHA256SUMS 到 `MushroomDAO/Cos72` Release）
     待 ME4-6.0.1 清单正式冻结后再做 —— 本片只做本地打包脚本 + 本地验证，不 push/开
     PR/打 tag/发 Release。
  正对照：把 `scripts/package.sh` 里 stage 阶段的 `cp domain-os.yml …` 一行删掉重跑 →
  tar 内容自检那步应失败并非 0 退出（已实测，见下方证据）。
- **涉及文件**：`scripts/package.sh`、`README.md`（新增「安装发布包」小节）、
  `docs/agent/tasks.md`。
- **证据**：2026-09-30 本地实测（Darwin arm64，rustc 1.95.0）：`bash scripts/package.sh`
  跑通，产出 `dist/cos72-0.1.0-macos-arm64.tar.gz`（2,877,771 字节）+ `dist/SHA256SUMS`；
  `shasum -a 256 -c SHA256SUMS` → `OK`；`tar -tzf` 恰好两个文件条目
  （`domain-os.yml`、`bin/cos72`，后者 `-rwxr-xr-x` 可执行）。`migrations/` 未纳入包 ——
  `src/store/mod.rs` 用的是编译期宏 `sqlx::migrate!("./migrations")`（非运行时
  `Migrator::new`），migrations 已嵌进二进制，脚本对此做了 grep 断言。`dist/` 未提交
  （已在 `.gitignore` 的既有 `dist` 规则覆盖范围内，未新增条目）。
