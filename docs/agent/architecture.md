# Cos72 架构 — 技术判断与骨架

> 「怎么搭」。定义契约与**不可动摇的边界**。数据细节见 [`spec.md`](spec.md)。
> 上游约束：Agent24 PLAN-ME4 §二 S4（硬约束下限，设计只能更严）、ME4-S3 §2.10（advise 与孤儿审批）、§3.5（`remember_once`）、
> SPEC-ME3（manifest、安装、挂载、威胁模型）。记录日期：2026-09-29。

## 核心判断

1. **技术选型与 Sin90 保持一致**：Rust（edition 2021，工具链 stable，SDK 的 MSRV 1.88）、axum 0.8、sqlx 0.8 + SQLite、tokio、
   `agent24-os-sdk = { git = "https://github.com/iDoris-ai/Agent24", tag = "agent24-os-sdk-v0.1.0" }`。理由：
   - 进程外模块的全部内核交互都在 SDK 里，而 SDK 是 Rust + axum（`Module::serve(axum::Router)`）；换语言就等于放弃 S3-4「Cos72 用 SDK 写成」这条验收。
   - 与 Sin90 同栈，Sin90 已踩过的坑（WAL/FK、`BEGIN IMMEDIATE`、outbox 泵、黑盒夹具、CI 形态）可以直接照搬，评审者（clestons）也不需要换语境。
   - 按 tag 而非分支钉 SDK：构建可复现，不随 Agent24 main 漂移；升级 tag 单独一个 commit（与 Sin90 相同约定）。
   - sqlx/SQLite：业务真相只在 `~/.agent24/os/cos72/cos72.db`（S4），单进程单写者，SQLite 足够且与内核同构。
2. **只依赖 SDK，不直接依赖 `agent24-os-proto`**。Cos72 用 SDK 默认的连接断开钩子（`warn!` + `exit(70)`，ME4-S3 §8 Q10），
   不需要命名 `FatalHook`；测试经 `agent24_os_sdk::testing`（`test-util`）拿假内核，`fake_kernel()` 返回的连接靠类型推断传给客户端构造函数。
   若实现中确需命名 `Connection`/`FatalHook`（SDK v0.1.0 的 crate 根**没有**转出 `FatalHook`，与 ME4-S3 §3.1 草图不一致），
   照 Sin90 迁移分支的做法加同 tag 的 `agent24-os-proto` 依赖，并在 `Cargo.toml` 注释写明理由。
3. **manifest 最小能力 = `[events, memory, approval]`**：
   - `events`：每个动作发事件（S4）。
   - `memory`：任务完成摘要写进 Cos72 自己的内核私有记忆（S4）。
   - `approval`：发积分经内核人工审批（S4、ME4-S3 §8 Q3 拍板用 advise）。
   - **不要 `scheduler`**：审批结果靠轮询 `status`，轮询是模块进程内的 tokio 定时循环，不是内核 cron；ME4-S3 §8 Q3 明确「SDK 不提供轮询助手，Cos72 在自己的 outbox/泵里轮询」。
     ME4-5.3.4 的 schedules 隔离按 ME4-S3 §8 Q4 的备选路径**从内核 REST 侧验**（开放问题 Q5）。
   - **不要 `models`**：原型没有任何推理需求（ME4-S3 §1.2 第 17 行「Cos72 不要」）。
   - `policy` 内核从不授予（`KERNEL_OOP_GRANTS` 不含），也不需要。
4. **发积分走 advise + 轮询 status，先落本地再提交**（ME4-S3 §2.10，写死）：
   submit handler 在**同一 SQLite 事务**里把任务改为 `submitted` 并插入 `awards` 行（`award_id` 唯一），提交后在**同一个 HTTP 请求内**
   调 `advise`（payload 带 `award_id`），拿到 `approval_id` 用 CAS 写回。入账只认 Cos72 自己记下的 `approval_id`，账本以 `award_id` 唯一约束入账——
   孤儿审批即使被批准也**只会少发、不会多发**。
5. **积分账本只追加，余额 = 回放**：没有 balance 列/表；`points_ledger` 由 SQLite 触发器拒绝 UPDATE/DELETE。`GET /points` 每次按账本求和。
6. **记忆写入只经 Cos72 自己的 outbox，由单一泵串行调 `remember_once`**（ME4-S3 §3.5：`remember_once` 非原子，同一 `dedup_key` 只允许一个写者；
   HTTP handler 与审批轮询器**都不许**直接调记忆客户端）。
7. **事件走 SDK 的 `EventSink`（即发即弃、满即丢）**，与 Sin90 相同；事件是通知不是真相（开放问题 Q6）。

## 系统骨架

```
Agent24 daemon ──(受约束代理 /api/v1/cos72/* , UDS fd 3)──► cos72 进程
     ▲                                                      │
     └──────(回调 socket A24_CALLBACK_SOCK：_a24/events|memory|approval)──┘

cos72 进程内：
  core/     纯类型 + 状态机转移函数（无 IO）：TaskStatus、AwardState、校验
  store/    sqlx：迁移、仓储；每次状态变化 BEGIN IMMEDIATE，同事务写 outbox
  http/     axum 路由（发布/领取/提交/查询/积分），只依赖 core + store + KernelPort
  kernel/   唯一碰 SDK 的地方：KernelPort 的 SDK 实现（advise/status/remember_once/emit）
  workers/  award_poller（轮询 status → 入账）、memory_pump（outbox → remember_once）
  main.rs   Module::builder(MANIFEST).connect() → 开库 → 起 workers → module.serve(router)
```

- `KernelPort` 是 Cos72 自己定义的窄 trait（`advise` / `approval_status` / `remember_once` / `emit`），SDK 客户端是它的生产实现，
  单测用内存假实现驱动状态机；另有少量测试用 `agent24_os_sdk::testing::fake_kernel` 钉 wire 形状（方法名、参数字段）。
- 一代进程只有一条回调连接（内核契约）；连接断 → SDK 默认 `exit(70)` → supervisor 起新一代。新一代启动时 poller/pump 从库里恢复进度，不依赖内存状态。

## 契约 / 接口（实现与调用分离）

| 方向 | 契约 | 说明 |
|:---|:---|:---|
| 外 → Cos72 | REST `/api/v1/cos72/*`（spec.md「REST 路由」） | 全部经内核代理；内核要求 daemon bearer token，代理剥掉 `Authorization` 与客户端伪造的 `X-A24-*` |
| Cos72 → 内核 | `_a24/events/emit` | kind 为点分小写两段以上（内核 `valid_kind`），不带模块前缀（内核按 manifest `name` 盖章） |
| Cos72 → 内核 | `_a24/approval/advise`、`_a24/approval/status` | advise 必须带当前被代理请求的 `X-A24-Request-Id` + `X-A24-Approval-Token`（SDK `RequestContext` 提取）；内核 TTL 300s |
| Cos72 → 内核 | `_a24/memory/private/remember`、`recall`（经 `remember_once`） | `dedup_key` 自带命名空间 `cos72:task:<task_id>:completed` |
| 人 → 内核 | `GET /api/v1/module-approvals`、`POST /api/v1/module-approvals/{id}` `{"decision":"approved"\|"denied"}` | Agent24 既有界面，Cos72 不实现审批 UI |
| 包 | `domain-os.yml` + `bin/cos72` | `agent24 os install <目录>`；manifest 字节与二进制里 `include_str!` 的是同一份（内核握手比对 digest） |

## 不可动摇的边界

1. **SQLite 是真相**。内核记忆里的摘要、事件流里的事件都是派生副本；丢了不影响任务与余额。
2. **涉钱（积分）操作幂等且只会少发**：入账必须同时满足「`awards.approval_id` 是 Cos72 自己记下的」「内核 `status` = `approved`」「`award_id` 在账本里尚不存在」，三者在一个事务里判定。
3. **advise 只在 submit 的 HTTP handler 内同步发**，绝不挪进 outbox/后台（token 随请求结束失效，ME4-S3 §2.10 第 1 条）；`Timeout` 用同一对 `{request_id, token}` 在请求内重试（wire 幂等）；`ConnectionLost`/`NotSent` 不重试。
4. **`remember_once` 只由 memory_pump 一个任务调用**；`Inconclusive` 不当作「不存在」，退避重试。
5. **账本只追加**：迁移里的触发器拒绝 UPDATE/DELETE；代码里不存在更新账本的语句（结构测试钉住）。
6. **只声明真正用到的能力**；代码按「句柄可能不在」写：`module.approval()` 为 `None` 时提交返回 503 `approval_unavailable`，不写任何本地状态。
7. **Cos72 从不碰 socket、从不自己解析协议帧**：源码里不出现 `UnixStream`/`UnixListener`/`from_raw_fd`/`serde_json::from_slice` 用于协议（结构检查钉住）。
8. **威胁模型照 SPEC-ME3 §0**：模块与 daemon 同 UID，「隔离」都是 broker API 之内的性质；不写比机制更强的措辞（PLAN-ME4 §一 第 10 条）。
9. **`/debug/*` 路由只在 `test-hooks` feature 下编译**，正式包不带（与 Sin90 相同）。

## 运行形态

- 独立 Rust 二进制 `cos72`，只有一种正式运行形态：由 agent24d spawn（`spawn.command: bin/cos72`，`args: ["module"]`），不提供 standalone 模式（开放问题 Q9）。
- 数据目录 `~/.agent24/os/cos72/`（内核按 name 派生，`A24_DATA_DIR`），库文件 `cos72.db`（WAL、`foreign_keys=ON`）。
- 后台任务两个：`award_poller`（每 3 秒扫一次 `awaiting` 且有 `approval_id` 的奖励）、`memory_pump`（outbox 单泵，退避 1s×2 上限 5min）。
- 分发：M2 起产出 tar.gz（`domain-os.yml` + `bin/cos72`）+ `SHA256SUMS`，发本仓库 GitHub Release（ME4-6.0.2）。
