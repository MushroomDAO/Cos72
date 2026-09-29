# Cos72 实时状态 — progress

> 「此刻仓库真实发生了什么」。由 `pilot run` 每一步更新。
> 更新时间：2026-09-29

## 当前聚焦
- **Milestone**：M1 原型：mytask + 审批发积分（Agent24 ME4-5.3）
- **Feature**：F1.0 规划层
- **正在开发的 Task**：T1.0.1 pilot 七件套 + `.pilot.yml`（ME4-5.3.1，状态：IN_PROGRESS——分支已推送，未开 PR）
- **分支 / worktree**：`docs/pilot-plan` / `~/Dev/mycelium/Cos72-plan`（从 `origin/main` `aaf9477` 切出）
- **PR**：未开（按指示只推分支；等 jason 过目开放问题后决定开 PR）

## 进行中 / 待回执的 PR
| Task | PR | 状态 | 备注 |
|:---|:---|:---|:---|
| T1.0.1 | — | 未开 | 分支 `docs/pilot-plan` |

## 阻塞项（BLOCKED）
- 无硬阻塞。T1.1.1 可在 T1.0.1 合并后开工；下列开放问题的**推荐选项**已写进 spec.md，jason 若改选，先改 spec.md/tasks.md 再开工。

## 待 jason 拍板的开放问题

| # | 问题 | 选项 | 推荐 |
|---|---|---|---|
| Q1 | 成员身份怎么来 | (a) 请求体 `member` 字符串，信任持 daemon token 的操作者；(b) 照 Sin90 做模块自己的 actor key（发布者/审批者/成员）；(c) 等 AirAccount/链上身份 | **(a)**：原型单操作者，审批本来就在内核；(b) 把范围翻倍且与内核审批职责重叠 |
| Q2 | 内核 advise 审批 TTL 固定 300s（`module_approval_broker.rs` `MODULE_APPROVAL_TTL_SECS`），社区审批常常 5 分钟内等不到人 | (a) 接受：过期 → `expired`，领取人再 submit 发新审批；(b) 给 Agent24 开 task 让 TTL 可按模块/按请求配置 | **(a)** 本轮；(b) 登记为 Agent24 followup，与 FU-84 一起做 |
| Q3 | 审批被拒后任务去哪 | (a) 回 `claimed`，同一领取人改后重交；(b) 回 `open`，任何人可再领；(c) 终态 `rejected` | **(a)**：最接近 MyTask 的「社区质疑后可重做」，且不需要新状态 |
| Q4 | 积分从哪来 | (a) 审批通过即铸造，发布者不预存、不托管；(b) 从发布者余额托管扣减（需要负数账本行与余额校验） | **(a)**：原型账本只追加正数；(b) 留给 myshop 一起设计扣减 |
| Q5 | 要不要申请 `scheduler` | (a) 不申请，schedules 隔离从内核 REST 侧 + Offer 验（ME4-S3 §8 Q4 允许）；(b) 仅为了模块侧隔离验证而申请 | **(a)**：最小能力原则；(b) 让 manifest 为测试而膨胀 |
| Q6 | 事件可靠性 | (a) SDK `EventSink` 即发即弃（与 Sin90 相同，满即丢）；(b) 事件也走 outbox，至少一次 | **(a)**：事件是通知不是真相；账与记忆已走 outbox/事务 |
| Q7 | 评审与保护 | PR-Daemon（clestons）是否已监听 `MushroomDAO/Cos72`；main 何时开 ruleset | 需要 jason 确认/手动开；未开前按 `.pilot.yml` 注释用 `gh pr merge --squash` + exact-head 判据 |
| Q8 | T1.3.1 是否预先拆成两个 PR | (a) 一个大片（≈1100–1400 行）；(b) 3b-1 advise+poller+账本 / 3b-2 outbox+记忆 | **(b)** 倾向：涉钱部分单独评审更干净；但 v4 原型规则允许 (a)，由 jason 定 |
| Q9 | 要不要 standalone 运行模式（不挂内核直接起端口） | (a) 不要；(b) 照 Sin90 提供 `serve --port` 调试模式 | **(a)**：没有内核就没有审批，standalone 走不通主流程 |

## 本分支实测记录
- `bash ~/.claude/skills/pilot/scripts/check-docs.sh --strict` → `ok=7/7`，退出码 0。
- 正对照：临时移走 `docs/agent/spec.md` → 同一命令退出码 1，`MISSING … docs/agent/spec.md`；已复原。

## 调研中核到的事实（供实现参考）
- SDK tag `agent24-os-sdk-v0.1.0` 存在，Agent24 main（v0.4.0，`bb1945d`）上 SDK/proto 与 tag 无差异。
- SDK crate 根**没有**转出 `FatalHook`（ME4-S3 §3.1 草图写了转出）；Cos72 用默认钩子即可不依赖 proto，否则照 Sin90 加同 tag proto 依赖。
- 内核 `advise` 的 `action` 不受闭集限制（只有 `gate` 受限）；事件 kind 须为点分小写两段以上、≤ 96 字节。
- 内核记忆**没有** REST 读口，黑盒只能经模块自己的 `recall`（Cos72 的 `test-hooks` 路由）核对。
- 审批决定入口：`POST /api/v1/module-approvals/{id}` `{"decision":"approved"|"denied"}`。
- 本仓库 `.gitignore` 是 Node 模板，没有 `/target`——T1.1.1 要补。

## 最近完成
- （无）

## 下一个 READY
- T1.1.1 骨架（ME4-5.3.2）——T1.0.1 合并后转 READY。
