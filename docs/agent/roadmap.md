# Cos72 Roadmap — Milestone → Feature

> 「未来要做什么」。具体怎么做+验收见 [`tasks.md`](tasks.md)。
> 编号：M<里程碑> → F<里程碑>.<序号>。跨仓库的门在 Agent24 `docs/agent/tasks.md`「ME-4 台账」（ME4-5.3.x）。记录日期：2026-09-29

## M1 — 原型：mytask + 审批发积分（= Agent24 ME4-5.3.2–5.3.4）
目标：用 `agent24-os-sdk` v0.1.0 写出第一个带人工审批与幂等入账的进程外领域 OS，并在真实 daemon 上与 Sin90 同时挂载、互不越界。

- **F1.0 规划层** — pilot 七件套 + `.pilot.yml`（ME4-5.3.1，本文件所在 PR）。
- **F1.1 骨架** — Cargo 工程、manifest、SDK 挂载、SQLite 迁移（全量 schema）、事件接线、CI、最小挂载冒烟（ME4-5.3.2）。
- **F1.2 mytask 实体与路由** — 发布 / 领取 / 提交（到 `submitted` 为止）/ 查询，状态机与事件（ME4-5.3.3a）。
- **F1.3 审批发积分 + 账本 + 记忆** — submit 内 advise、pending_award 孤儿补偿、status 轮询入账、追加式账本回放、outbox 串行 `remember_once`（ME4-5.3.3b）。
- **F1.4 真实挂载黑盒** — 安装 → 挂载 → 全流程 → 审批往返 → 重启回放 → 与 Sin90 共存隔离（ME4-5.3.4）。

## M2 — 可安装发布（= Agent24 ME4-6.0.2，v0.5.0 发布物之一）
目标：Mac mini 干净机器上只用已发布资产就能装上 Cos72 并挂载。

- **F2.1 打包与 Release** — 构建脚本产出 `cos72-<ver>-<target>.tar.gz`（`domain-os.yml` + `bin/cos72`）+ `SHA256SUMS`，发本仓库 GitHub Release；具体文件名以 ME4-6.0.1 冻结的清单为准。
- **F2.2 干净机验收配合** — ME4-6.1.3 在 Mac mini 上下载 → 校验 → 解压 → `agent24 os install` → `mounted`。

## M3 — 社区能力扩展（未排期，不在 ME-4 本轮）
目标：从「记账」走向「用账」，对应 Agent24 roadmap M5 与 AAStar Cos72 的其余模块。

- **F3.1 myshop** — 用积分兑换（账本出现 `delta < 0`，需要余额校验与扣减的审批）。
- **F3.2 myvote** — 社区投票。
- **F3.3 成员身份与多操作者** — 取代「请求体 `member` 字符串」的信任模型（开放问题 Q1 的后续）。
- **F3.4 渠道接入** — 经 Agent24 的微信 / Nostr 通道发布与领取任务。
- **F3.5 链上桥接** — 与 `MushroomDAO/MyTask` 合约 / AirAccount 对接（依赖 AAStar 集成计划）。
- **F3.6 审批裁决回推** — Agent24 FU-84 落地后，把 status 轮询换成内核回推，根治孤儿审批。

---

> 当前聚焦：M1 / F1.0（本 PR）→ 下一个 READY 在依赖满足后为 F1.1 的 T1.1.1。每个 Feature 的 Task 拆分与状态见 tasks.md。
