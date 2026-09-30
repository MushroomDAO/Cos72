# Cos72 立项调研 — research

> 「为什么做、凭什么做」。本仓库是 Agent24 ME-4 轮 S4 的落点（`MushroomDAO/Cos72`，D5/D6 裁决）。
> 记录日期：2026-09-29。权威来源：Agent24 `docs/agent/PLAN-ME4-OS-CAPABILITIES.md`（下称 PLAN-ME4）§二 S4、
> `docs/design/ME4-S3-os-sdk.md`（下称 ME4-S3，v4 冻结）§1.2 / §2.10 / §3.5 / §8。

## 五步框架

1. **要解决的问题** —
   - **对 Agent24（主因）**：`agent24-os-sdk` v0.1.0 是从 Sin90 这**一个**真实模块 + 一个 Python 黑盒里提取的。
     PLAN-ME4 §〇 明说「SDK 等有了第二个真实模块（Cos72）再从两个真实调用方身上提取」，S3-4 的验收是「Cos72 用 SDK 写成」。
     没有 Cos72，SDK 的 `ApprovalClient` 没有任何业务调用方（ME4-S3 §1.2 第 15 行：Sin90 只有 test-hooks 路由在用），
     `remember_once` 只有一个调用方，FU-87（`retry_class` 是否提取）永远无法裁决。
   - **对社区（产品面）**：一个小社区要能「发任务 → 有人领 → 交付 → 管理者确认 → 记积分」，且积分账可审计、可重算。
     这是 AAStar Cos72 的 mytask 在链上做的事；本仓库先在 Agent24 内核上做一个**链下、单操作者**的最小版本。
2. **现有方案全景** — 见下表。
3. **差异化立足点** — 不是做一个更好的任务市场，而是做**第一个用 SDK 从零写成、用到审批的进程外领域 OS**，
   证明「给 manifest + 给一个 axum Router + 声明能力」就能写出一个带人工审批、带幂等入账、带派生记忆的模块。
4. **可复用 vs 要自建** —
   - 复用：`agent24-os-sdk`（握手、帧、五个客户端、`remember_once`、`RequestContext`、`test-util` 假内核）；
     Sin90 的工程形态（Rust + axum + sqlx/SQLite、单主干 pilot、`#[ignore]` 真实挂载黑盒、outbox 单泵）；
     AAStar/MyTask 的**业务语义**（任务状态机、积分奖励）。
   - 自建：mytask 表与状态机、pending_award + 轮询 + 追加式账本、Cos72 自己的 outbox 泵、真实挂载黑盒。
5. **License / 合规边界** — 本仓库 Apache-2.0（LICENSE/NOTICE/TRADEMARK 已在 main）。依赖 `iDoris-ai/Agent24`（公开仓库）的
   SDK 走 git tag 依赖；只借鉴 AAStar/MyTask 的**语义**，不复制其代码（那边是 TS/Solidity，这边是 Rust，也无代码可复制）。
   不接链、不涉真实资产：积分是社区内部记账单位，不可转让、不可兑换（原型阶段）。

## 开源 / 生态全景表

| 项目 | 能力 | 可借鉴 | License |
|:---|:---|:---|:---|
| `AAStarCommunity/Cos72`（本地 `~/Dev/aastar/Cos72`） | Web3 社区 OS：mytask/myshop/myvote 前后端 + 合约集成 | 模块划分（mytask 先行、myshop/myvote 后置）、产品名 | Apache-2.0 |
| `MushroomDAO/MyTask`（本地 `~/Dev/mycelium/MyTask`） | 链上任务托管 `TaskEscrowV2`：`Open → Accepted → InProgress → Submitted → Challenged → Finalized / Refunded`，社区 `approveWork` 放款 | 状态机骨架（发布 → 领取 → 提交 → 社区批准 → 发放）；「社区批准」对应本仓库的内核人工审批 | 见该仓库 |
| `iDoris-ai/Sin90`（本地 `~/Dev/auraai/sin90-design`） | 第一个进程外领域 OS（Rust），ME4-5.2.1 正迁到 SDK（`feat/ts1.1-migrate-to-sdk`） | manifest 写法、`Module::builder(..).connect()` 用法、outbox 泵与错误分类、`agent24_mount_blackbox.rs` 的装包/起 daemon/WS 订阅夹具 | Apache-2.0 |
| `iDoris-ai/Agent24` `agent24-os-sdk` v0.1.0 | 进程外模块 SDK | 全部内核交互 | Apache-2.0 |

## 结构性空白（差异化）

- SDK 的 **advise 路径**从没被真实业务走过：`advise` 只在被代理请求存活期间有效、结果要靠轮询 `status`、
  内核没有「按内容列出我的审批」的方法——这些约束（ME4-S3 §2.10）只有一个真实业务才能证明写得出来、写得对。
- 「审批通过才入账、且孤儿审批**只会少发不会多发**」是涉钱语义的最小样板，后续 myshop（扣积分）会直接复用。
- 两个进程外模块**同时挂载**时的记忆/调度隔离（ME4-5.3.4）至今没有黑盒证据。

## 结论

做。范围按 D6 与 jason 的硬要求压到最小：**mytask（发布/领取/提交/完成）+ 审批发积分 + 追加式积分账本 + 完成摘要进记忆 + 事件**，
manifest 只声明 `events / memory / approval`。第一个里程碑 = ME4-5.3.2–5.3.4 全绿（`cargo test` + 真实挂载黑盒），
它是 Agent24 v0.5.0（ME4-6.0.1 发布清单）的前置。myshop / myvote / 渠道接入 / 链上桥接不在本轮。
