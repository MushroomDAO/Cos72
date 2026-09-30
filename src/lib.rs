//! Cos72 — 社区任务与积分领域 OS，跑在 Agent24 内核之上（ME4-5.3 原型）。
//!
//! T1.1.1（ME4-5.3.2）搭了骨架：manifest + SDK 挂载 + 迁移 + 事件。T1.2.1
//! （ME4-5.3.3a）加 mytask 实体与路由：发布/领取/提交/查询，任务走到
//! `submitted`（只落本地 `awaiting` 奖励行，advise/轮询/账本/记忆是
//! T1.3.1，docs/agent/tasks.md T1.2.1「明确不做」）。
//!
//! ```text
//! core/     纯类型 + 状态机转移函数（无 IO）：TaskStatus、校验
//! kernel/   唯一碰 SDK 的地方：KernelPort 的 SDK 实现（emit 已实现；advise /
//!           approval_status / remember_once 占位，T1.3.1 补齐）
//! store/    sqlx：迁移 + 连接池 + 仓储（`store::tasks`，T1.2.1 起）
//! http/     axum 路由：GET /health + mytask 路由（`http::tasks`，T1.2.1 起）
//! ```

pub mod core;
pub mod http;
pub mod kernel;
pub mod store;
pub mod workers;

/// The manifest bytes sent to the kernel at `initialize` — the SAME bytes
/// `tests/manifest.rs::binary_embeds_the_same_manifest_bytes` compares
/// against a runtime read of `domain-os.yml` (docs/agent/tasks.md T1.1.1
/// 验收命令 #1). `main.rs` uses this constant, not a second, independent
/// `include_str!` of its own — see Sin90's `main.rs` MANIFEST doc for why a
/// second embed is a foot-gun this crate deliberately avoids from the start.
pub const MANIFEST: &str = include_str!("../domain-os.yml");
