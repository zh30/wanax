# 蚁后（Wanax）Implementation-Ready Spec

| 字段 | 值 |
|---|---|
| Document Title | 蚁后（Wanax）Lights-Off Software Factory |
| Product name | 蚁后 |
| English name | Wanax |
| Crate / CLI | `wanax` |
| Brand token | `wanax`（5 字母；crates.io / npm 精确包名均未占用，检索日 2026-08-30） |
| Former working names | NightForge、antqueen、Ant Queen（均已废弃，禁止出现在代码、路径、环境变量） |
| Version | 0.1.0 |
| Date | 2026-08-30 |
| Status | Draft — Implementation-Ready for Phase 1 |
| Author | Henry Zhang / Grok Team |
| Audience | AI Coding Agent + Human implementer |
| Language | 说明中文；标识符、schema、CLI、FR 编号一律英文 |
| Source inspiration | @blackanger OctoLoop + agent-spec CBV；对照 Orbi issue→PR 工厂 |

---

## How to Use This Spec with AI Agents

1. 将本文件保存为项目中的 `docs/PRD.md` 或 `specs/PRD.md`。
2. 在 Cursor / Claude Code / Grok Build 中使用 `@docs/PRD.md` 引用。
3. 推荐执行指令：
   - “请严格根据 @docs/PRD.md 实现 Phase 1。只实现 Phase 1 范围内的内容，完成后对照 Verification Checklist 中 Phase 1 部分自检。不要添加 Spec 中未提及的任何功能。”
   - “根据 PRD 中的 Data Model 先创建 Rust types、serde schema 和 SQLite migrations。”
   - “先实现 Tombstone schema + Run state machine + 单 Worker adapter，不要先做 TUI。”
4. 按 Phase 逐步推进，或针对特定 FR-xxx 进行迭代。
5. 任何与 Spec 冲突的实现决策，优先以本 Spec 为准；出现 `[NEEDS CLARIFICATION]` 时停下询问，禁止自行发明产品行为。
6. Agent 不得实现本 Spec 中未明确要求的功能。如有歧义，优先做更少的功能。

---

## Executive Summary

蚁后是一个 **本地、单用户、Rust 实现的关灯软件工厂运行时**。

中文产品名是蚁后。对外品牌词和 CLI 是 `wanax`（迈锡尼希腊语「君主」），不走 ant/queen/hive 热词。领域对象仍用 `FactoryRun` / `Contract` / `Tombstone` / `WorkUnit`，禁止把规格写成寓言。

它不是又一个 coding agent。它是一层 **工厂控制面**：

- 人只写 Task Contract（意图 / 决策 / 边界 / 完成条件），然后启动一次 `FactoryRun`。
- **内环（Inner Loop）** 用便宜模型驱动现成 coding agent 写代码、跑测试、提交回执。
- **外环（Outer Loop）** 用贵模型只做三件事：派活、独立复测、盖章。
- 两环只通过 **Tombstone（墓碑文件 + JSON 信封）** 通信。外环是唯一拥有 `git push` / 开 PR / 合入受保护分支权限的角色。
- 任何进主线的变更必须同时满足：边界检查通过、绑定测试在外环干净 worktree 复跑通过、外环 Verdict=`accept`。

一句话目标：**Spec in → 机械验证 → 可审计 PR out。人可以离开键盘。人不能离开契约。**

默认假设（可被用户推翻，见开放问题）：

- v1 是本地前台 CLI。不实现常驻 daemon；需要无人值守时由用户用 `tmux`/`systemd --user` 自己挂 `wanax start`。
- 工人不从零写，通过 `WorkerAdapter` 调用 `octoscode` / `claude` / `codex` / 任意 CLI agent。
- 目标仓库默认是 Git 仓库；GitHub PR 为 P2，P0 只产出本地分支。
- 验证默认命令是仓库内可配置的 `test_command`（Rust 项目默认 `cargo test`）。

---

## Problem Statement & Goals

### 问题

单模型长程编码有三个结构性失败，不是 prompt 写得不好：

1. **贵模型被苦力活污染上下文**：Fable 5 / GPT-5.6 去读文件、改分号、重跑测试，决策次数被耗尽。
2. **便宜模型会撒谎**：自称测试通过、自称没越界。没有独立复测，主线会被污染。
3. **人仍是环路瓶颈**：每个中间 diff 都要人看，工厂关不了灯。人看 diff 的速度已经追不上 agent 产代码的速度。

@blackanger 的 OctoLoop 给出的解法是对的：**贵模型当指挥和验收，便宜模型当班组，墓碑传话，外环独占推送权。** agent-spec 给出的另一半也是对的：**人审契约，机器验符合性（CBV）。**

蚁后把这两半焊成一个可运行的控制面。

### 目标

| ID | Goal | 量化成功标准 |
|---|---|---|
| G-1 | 一次合格契约可以无人值守跑完 | Phase 1：单 WorkUnit 从 `dispatched` 到 `accepted` 或 `rejected`，中途 0 次人工 tool-approval |
| G-2 | 外环独立复测不可绕过 | 内环回执声称 pass、外环复测 fail 时，禁止 accept；100% 被测用例命中 |
| G-3 | 越界改动不可合入 | 不在 `allowed_globs` 内的路径变更 → `verdict=reject`，exit code 非 0 |
| G-4 | 成本受控 | 单 Run 可设 `max_usd` 与 `max_inner_turns`；超限进入 `budget_exhausted`，不继续派活 |
| G-5 | 审计可回放 | 任意 Run 可从 Tombstone 重建：谁在何时派了什么、测了什么、判了什么 |
| G-6 | 工人可替换 | 至少 1 个真实 CLI adapter（`octoscode`）+ 1 个 `fake` adapter 用于单测 |

### 非目标指标（v1 不承诺）

- 不承诺 MRR / ARR。v1 不是商业 SaaS。
- 不承诺“自主完成任意产品”。没有合格契约的任务，工厂必须拒绝启动，而不是硬跑。

---

## Target Users & Personas

v1 用户不是大众消费者。是已经在用 coding agent 的独立开发者 / 小团队 tech lead。

| Persona | 行为 | 要什么 | 不要什么 |
|---|---|---|---|
| P1 Solo Rust/TS 开发者 | 晚上丢一个模块改造，早上收 PR | 便宜模型把活干完，贵模型别把 token 烧在 grep 上 | 再学一套新 IDE / 再养一个 20 微服务平台 |
| P2 小团队 Tech Lead | 把“可关灯”的任务从人审 diff 里剥离 | 边界 + 测试门 + 审计日志 | Agent 自己改基础设施、自己 bump 依赖、自己推生产 |
| P3 Agent 作者（dogfood） | 用工厂自举工厂 | 稳定状态机、可单测的 adapter | 把编排和工人耦死在一个二进制里 |

海外对应场景：和 Claude Code `/loop`、Codex unattended、Orbi issue→PR 同类用户。差异是蚁后 **强制外环复测 + 墓碑协议 + 契约门**，不是“开个 agent 挂着”。对外文档用 Wanax；仓库、crate、CLI 一律 `wanax`。

---

## Domain Glossary

| Term | Definition | 不变量 |
|---|---|---|
| 蚁后 | 中文产品显示名。控制面本身，不是某个 LLM persona | 面向人的称呼用蚁后 |
| Wanax / `wanax` | 英文品牌词与 crate/CLI 名 | 代码、路径、环境变量只用 `wanax`。禁止 NightForge / antqueen |
| Commander | 外环决策角色。对应隐喻里的蚁后个体 | 每个 Run 同一时刻 1 个 Commander session。Commander 不写业务代码 |
| FactoryRun | 一次关灯执行实例，绑定 1 个 Contract + 1 个目标 Git repo + 预算 | 同一时刻同一 `repo_root` 只允许 1 个 active Run（P0）。P1 才允许多 Run 分 worktree |
| Contract | 任务契约。四段：`intent` / `decisions` / `boundaries` / `completion_criteria` | 启动后 Contract 文件 hash 冻结。变更必须开新 Run |
| WorkUnit | Contract 拆出的最小可验收单元 | 必须绑定 ≥1 条 `test_command` 或显式 `no_test` 且由外环书面批准（P0 禁止 `no_test`） |
| Inner Loop | 便宜模型 + WorkerAdapter 执行实现 | 内环 **不得** 执行 `git push`、不得修改受保护分支、不得改 Tombstone 的 `outer_*` 字段 |
| Outer Loop | 贵模型指挥 + 独立复测 + 盖章 | 外环 **不得** 在目标 worktree 里写业务代码。外环只写 Tombstone 与编排元数据 |
| Master | 内环班长，拆活、调度 Goal/Peer | P0 无 Peer 时 Master 自己干 |
| GoalAgent | 对单一目标循环：plan → edit → test → self_review → score，直到达标或预算耗尽 | 自审模型必须 ≠ 实现模型（若只配了 1 个 inner model，自审降级为“重跑测试 + diff 清单”，不得自称 semantic review pass） |
| PeerAgent | 并行分身，每人独立 worktree | P1。禁止共享同一 worktree 写文件 |
| Tombstone | Run 的唯一跨环通信与审计账本 | 人类可读 Markdown + 机器可读 JSON envelope，二者必须同源生成，禁止手改 JSON 而不更新 Markdown |
| Receipt | 内环完工回执 | 必须包含：changed_files、test_command、test_exit_code、test_excerpt、diffstat、commit_sha |
| Verdict | 外环裁决 | 枚举：`accept` \| `reject` \| `rework` \| `escalate`。`accept` 的前置条件见 FR-014 |
| WorkerAdapter | 把“执行一个 goal”适配到外部 coding agent CLI | adapter 只暴露 `start` / `status` / `cancel` / `collect_receipt` |
| IsolatedWorktree | `git worktree add` 出的独立工作区 | 外环复测必须在 **新的** worktree 或 reset 后的干净树上跑，禁止复用内环脏树 |
| Budget | 美元与 turn 双上限 | 任一上限触达即停派活。已在跑的 worker 收到 cancel，最多等待 `cancel_timeout_secs` |
| ProtectedRef | 默认 `main`/`master` | 内环永远不能 checkout 或 push 这些 ref |

---

## Scope

### In Scope（Phase 1–3 合计）

- Rust CLI：`wanax init|start|status|cancel|verdict|doctor`
- Contract 文件格式（Markdown + YAML front matter）
- Run 状态机 + SQLite 持久化 + Tombstone 落盘
- 单 Inner WorkerAdapter（`octoscode` + `fake`）
- Outer 独立复测（干净 worktree 执行 `test_command`）
- 边界检查（path glob allow/deny）
- 预算熔断（USD + turns）
- 结构化日志与 Tombstone 审计
- Phase 2：Goal 循环与自审降级规则
- Phase 3：Peer + worktree 隔离 + 结果回收

### Out of Scope

- 多租户 SaaS、账号体系、计费后台
- Web Dashboard / 移动端
- 自动部署到生产
- 形式化验证（Lean/coq）
- 自动从一句话需求生成高质量 Contract（可草稿，不可不经人确认就开跑）
- 自己实现完整 coding agent（文件编辑循环、LSP、MCP 生态）
- Slack/Telegram 控制面（Future）
- Windows 一等支持（P0 目标 OS：Linux / macOS）

### Non-Goals（Agent 禁止实现）

- 禁止做聊天机器人前端。
- 禁止在 v1 实现“自我进化 prompt”或在线改系统提示而不写 Tombstone。
- 禁止给内环 `git push` 凭证。
- 禁止静默 `git commit --amend` 外环已验收的 commit。
- 禁止扫描或上传仓库以外的 `$HOME` 文件。
- 禁止实现支付、云同步、用户成长体系。
- 禁止把 LLM 原始全程 transcript 默认上传任何远程日志服务。

### Future Considerations

- GitHub Issue 为需求源（Orbi 模式）
- `agent-spec` CLI 作为 L1–L3 验证器插件
- Commander 可插拔：Claude Code / Codex / Fable 5
- 多 Run 并行（需 repo 级锁升级为 path-set 锁）
- 成本看板与模型路由策略学习

---

## Architecture Logic（给实现者的硬约束）

```
Human
  │ 1. write Contract
  │ 2. wanax start
  ▼
wanax (control plane, Rust)
  ├─ RunStateMachine
  ├─ BudgetAccountant
  ├─ TombstoneStore
  ├─ GitIsolation (worktree / fetch / diff / commit)
  ├─ Verifier (boundaries + test runner)
  ├─ OuterCommander (LLM, no repo writes except tombstone)
  └─ WorkerSupervisor
        ├─ Adapter::Octoscode
        ├─ Adapter::ClaudeCli    [P2]
        ├─ Adapter::CodexCli     [P2]
        └─ Adapter::Fake         [test]
              ▼
        IsolatedWorktree (inner)
              ▼
        Receipt → Tombstone
              ▼
        IsolatedWorktree (outer retest, clean)
              ▼
        Verdict
              ▼
        Optional: open PR (P1) / write local branch only (P0)
```

### 五条不可违反的工厂法

1. **推送权只在外环。** 内环 git 远程凭证为空。测试中若内环能 push 成功，视为 P0 缺陷。
2. **外环不写业务代码。** Commander 输出只能是：WorkUnit 指令、Verdict、Tombstone 字段、调度命令。
3. **外环复测必须换树。** 禁止在内环 worktree 上跑“验收测试”当作外环测试。
4. **墓碑是唯一跨环通道。** 禁止 Commander 直接读内环 session transcript 作为验收依据。Transcript 只用于 escalate 调试，不用于 accept。
5. **契约冻结。** Run 开始后改 Contract 必须 `cancel` 旧 Run 再 `start` 新 Run。

### 为什么不是一个 Agent

一个 Agent 自己写、自己测、自己说 pass，这叫自嗨，不叫工厂。  
工厂的最小单元是 **两个独立主体 + 一份不可抵赖的中间账本 + 一门机械测试**。

---

## Data Model

存储：

- SQLite：`~/.wanax/wanax.db`（或 `--data-dir`）
- Tombstone 文件：`<repo>/.wanax/runs/<run_id>/TOMBSTONE.md` 与 `envelope.json`
- 工作树：`<repo>/.wanax/worktrees/<run_id>-<role>/`

ID 规则：所有 ID 为 `wx_` 前缀 + ULID（26 char Crockford）。例：`wx_01K3Q...`

时间：一律 RFC3339 UTC。

金钱：USD，4 位小数，IEEE 不直接存 float；用整数 `usd_micros`（1 USD = 1_000_000）。

### Entity: Contract

| Field | Type | Required | Validation | Default | Notes |
|---|---|---|---|---|---|
| id | string | yes | `^wx_[0-9A-HJKMNP-TV-Z]{26}$` | generated | PK |
| path | string | yes | repo 内相对路径，`.md` | — | 通常 `specs/<name>.contract.md` |
| content_sha256 | string | yes | 64 hex | — | 冻结依据 |
| intent | string | yes | 1–4000 chars | — | |
| decisions | string[] | yes | 1–50 items, each 1–500 chars | — | |
| allowed_globs | string[] | yes | 1–200 gitignore-style globs | — | 至少一条 |
| forbidden_globs | string[] | yes | 0–200 | `["**/.env", "**/.wanax/credentials*"]` | 命中即 reject |
| forbidden_rules | string[] | no | 0–50 natural language | [] | 仅供外环审，不作为 P0 机械门 |
| completion_criteria | CompletionCriterion[] | yes | 1–30 | — | |
| test_command | string | yes | 非空，禁止 `rm -rf` / `sudo` / 管道写绝对路径见 FR-021 | — | 在 worktree 根执行 |
| test_timeout_secs | u32 | yes | 10–3600 | 300 | |

### Entity: CompletionCriterion

| Field | Type | Required | Validation | Default | Notes |
|---|---|---|---|---|---|
| id | string | yes | `CC-[0-9]{2,3}` | — | |
| statement | string | yes | 1–300 chars | — | Given/When/Then 或一句话断言 |
| bound_test | string | no | 测试名或过滤器 | null | 例 `test_register_api_returns_201` |
| must_have_files | string[] | no | globs | [] | |

### Entity: FactoryRun

| Field | Type | Required | Validation | Default | Notes |
|---|---|---|---|---|---|
| id | string | yes | nf ULID | generated | PK |
| repo_root | string | yes | 绝对路径，必须是 git work tree | — | |
| contract_id | string | yes | FK Contract | — | |
| contract_sha256 | string | yes | 与启动时一致 | — | |
| state | enum | yes | 见状态机 | `draft` | |
| base_sha | string | yes | 40 hex | — | 启动时 HEAD |
| inner_branch | string | yes | `wanax/<run_id>/inner` | generated | |
| outer_branch | string | yes | `wanax/<run_id>/outer` | generated | |
| commander_model | string | yes | 非空 | from config | |
| inner_model | string | yes | 非空 | from config | |
| reviewer_model | string | no | 可空 | inner_model 时降级 | Goal 自审 |
| max_usd_micros | i64 | yes | 0–100_000_000 | 5_000_000 | 默认 $5 |
| max_inner_turns | u32 | yes | 1–500 | 40 | |
| spent_usd_micros | i64 | yes | ≥0 | 0 | |
| spent_inner_turns | u32 | yes | ≥0 | 0 | |
| worker_adapter | enum | yes | `octoscode` \| `fake` \| `claude` \| `codex` | `octoscode` | P0 实现前两个 |
| created_at | datetime | yes | RFC3339 | now | |
| updated_at | datetime | yes | RFC3339 | now | |
| finished_at | datetime | no | | null | |
| last_error | string | no | ≤2000 | null | |

### FactoryRun.state

```
draft → contract_ready → dispatched → inner_working → receipt_ready
      → outer_reviewing → accepted | rejected | rework | escalate
                                           ↺ rework → dispatched
      任意非终态 → canceling → cancelled
      任意非终态 → budget_exhausted
      任意非终态 → failed
```

终态：`accepted` `rejected` `cancelled` `budget_exhausted` `failed` `escalate`

`rework` 不是终态。同一 WorkUnit 最多 `max_rework=3`，超过 → `escalate`。

### Entity: WorkUnit

| Field | Type | Required | Validation | Default | Notes |
|---|---|---|---|---|---|
| id | string | yes | nf ULID | generated | |
| run_id | string | yes | FK | — | |
| seq | u32 | yes | 从 1 递增 | — | |
| title | string | yes | 1–120 | — | |
| instruction | string | yes | 1–8000 | — | 写给内环的任务单 |
| state | enum | yes | `queued` `assigned` `implementing` `self_verifying` `receipt_ready` `outer_testing` `accepted` `rejected` `blocked` | `queued` | |
| assignee_role | enum | yes | `master` `goal` `peer` | `master` | P0 只用 master |
| parent_id | string | no | | null | P1 peer 用 |
| rework_count | u32 | yes | 0–3 | 0 | |
| inner_commit_sha | string | no | 40 hex | null | |
| receipt_id | string | no | FK | null | |
| verdict_id | string | no | FK | null | |

### Entity: TombstoneEnvelope（`envelope.json`）

| Field | Type | Required | Validation | Default | Notes |
|---|---|---|---|---|---|
| schema_version | string | yes | semver，P0=`1.0.0` | `1.0.0` | |
| run_id | string | yes | | | |
| contract_sha256 | string | yes | | | |
| events | TombstoneEvent[] | yes | append-only | [] | |
| current_state | string | yes | 与 DB 一致 | | |

### Entity: TombstoneEvent

| Field | Type | Required | Validation | Default | Notes |
|---|---|---|---|---|---|
| id | string | yes | nf ULID | | |
| at | datetime | yes | RFC3339 | | |
| actor | enum | yes | `human` `commander` `master` `goal` `peer` `verifier` `system` | | |
| kind | enum | yes | `run_started` `unit_dispatched` `receipt_submitted` `outer_test_started` `outer_test_finished` `verdict` `budget_tick` `state_changed` `error` `cancelled` | | |
| payload | object | yes | 按 kind 分 schema | | |
| payload_sha256 | string | yes | hash(canonical JSON) | | |

`TOMBSTONE.md` 由 envelope 渲染，禁止反向解析 Markdown 作为真源。真源是 `envelope.json`。

### Entity: Receipt

| Field | Type | Required | Validation | Default | Notes |
|---|---|---|---|---|---|
| id | string | yes | nf ULID | | |
| work_unit_id | string | yes | | | |
| changed_files | string[] | yes | 相对路径，0–500 | | 0 文件且非 rework 取消 → reject |
| diffstat | string | yes | ≤4000 | | `git diff --stat` |
| commit_sha | string | yes | 40 hex | | 必须是 inner_branch 后代 |
| test_command | string | yes | 等于 Contract.test_command | | 不一致 → reject |
| test_exit_code | i32 | yes | | | |
| test_excerpt | string | yes | 最后 ≤8000 chars | | |
| claimed_pass | bool | yes | | | 内环自称 |
| duration_ms | u64 | yes | | | |
| adapter | string | yes | | | |
| raw_artifact_path | string | no | 相对 `.wanax/runs/<id>/artifacts/` | | |

### Entity: Verdict

| Field | Type | Required | Validation | Default | Notes |
|---|---|---|---|---|---|
| id | string | yes | nf ULID | | |
| work_unit_id | string | yes | | | |
| decision | enum | yes | `accept` `reject` `rework` `escalate` | | |
| reason | string | yes | 1–2000 | | |
| outer_test_exit_code | i32 | yes | | | accept 必须 == 0 |
| outer_test_excerpt | string | yes | ≤8000 | | |
| boundary_ok | bool | yes | | | accept 必须 true |
| files_reviewed | string[] | yes | | | |
| commander_model | string | yes | | | |
| created_at | datetime | yes | | | |

### Entity: Config（`~/.wanax/config.toml` 或 repo `.wanax/config.toml`，repo 覆盖全局）

| Field | Type | Required | Validation | Default | Notes |
|---|---|---|---|---|---|
| commander.provider | string | yes | `anthropic` `openai` `openai_compat` | — | [NEEDS CLARIFICATION] 用户密钥源 |
| commander.model | string | yes | 非空 | — | |
| inner.provider | string | yes | 同上 | — | |
| inner.model | string | yes | 非空 | — | |
| reviewer.model | string | no | | | 空则自审降级 |
| worker.adapter | string | yes | `octoscode` `fake` | `octoscode` | |
| worker.octoscode_bin | string | no | which | `octoscode` | |
| worker.timeout_secs | u32 | yes | 30–14400 | 1800 | |
| budget.max_usd | decimal string | yes | | `5.00` | 写入 DB 转 micros |
| budget.max_inner_turns | u32 | yes | | 40 | |
| git.protected_refs | string[] | yes | | `["main","master"]` | |
| test.default_command | string | no | | `cargo test` | |
| lock.repo_exclusive | bool | yes | | true | P0 必须 true |

密钥只来自环境变量：`WANAX_COMMANDER_API_KEY`、`WANAX_INNER_API_KEY`。禁止写入 git。禁止写入 Tombstone。

### Entity: RepoLock

| Field | Type | Required | Validation | Default | Notes |
|---|---|---|---|---|---|
| repo_root_real | string | yes | canonicalize | | |
| run_id | string | yes | | | |
| lock_path | string | yes | `<repo>/.wanax/LOCK` | | flock |

---

## Functional Requirements

### FR-001 初始化仓库工厂目录

- Priority: P0
- User Story: 作为开发者，我运行 `wanax init`，以便在目标仓库生成契约模板与 `.wanax/` 结构。
- Given 目标路径是 git repo 且尚未初始化
- When 执行 `wanax init`
- Then 创建：
  - `.wanax/config.toml`
  - `.wanax/.gitignore`（忽略 `worktrees/` `LOCK` `*.db` `credentials*`）
  - `specs/example.contract.md`
  - 将 `.wanax/config.toml` 与 `specs/` 提示加入 git（不自动 commit）
- 正向：空 git repo → exit 0，上述文件存在
- 负向：非 git 目录 → exit 2，stderr=`E_NOT_GIT`，不写任何文件
- 错误：已存在且非 `--force` → exit 3，`E_ALREADY_INIT`

### FR-002 Contract 校验门

- Priority: P0
- User Story: 作为开发者，不完整的契约不能开跑。
- Given 契约缺 `allowed_globs` 或 `test_command` 或 `completion_criteria` 为空
- When `wanax start --contract <path>`
- Then exit 4，`E_CONTRACT_INVALID`，列出缺失字段，不创建 Run
- 正向：完整契约 → 进入 `contract_ready`
- 负向：`allowed_globs` 为空数组 → 无效

### FR-003 启动 Run 并冻结契约

- Priority: P0
- User Story: 我启动一次关灯执行，系统记下当前 HEAD 与契约 hash。
- Given 有效契约 + repo 无 active lock
- When `wanax start --contract specs/foo.contract.md`
- Then：
  1. 计算文件 sha256 写入 Run
  2. 记录 `base_sha=HEAD`
  3. 创建 inner/outer branch（基于 base）
  4. 写 LOCK
  5. 写 Tombstone `run_started`
  6. state=`dispatched`
- 正向：输出 `run_id`
- 负向：工作区有非 `.wanax` 的未提交脏文件且未 `--allow-dirty` → exit 5，`E_DIRTY_WORKTREE`

### FR-004 仓库互斥锁

- Priority: P0
- Given 已有 active Run
- When 再 `start`
- Then exit 6，`E_REPO_LOCKED`，打印现有 `run_id`
- 锁实现：`fs2`/`file-guard` flock + `LOCK` 内容含 run_id、pid、started_at
- 负向：进程崩溃留下 LOCK 但 pid 不存在 → `wanax doctor --fix-lock` 可清；`start` 本身不得悄悄清锁

### FR-005 外环派活写入墓碑

- Priority: P0
- Given Run=`dispatched` 且尚无 WorkUnit
- When Commander 生成恰好 1 个 WorkUnit（Phase 1 限制：一个 Run 一个 WorkUnit）
- Then Tombstone 追加 `unit_dispatched`，state=`inner_working`
- 正向：instruction 含边界摘要 + test_command + completion_criteria
- 负向：Commander 输出无法解析为 WorkUnit schema → 重试最多 2 次，仍失败 → `failed` + `E_COMMANDER_SCHEMA`

Phase 1 硬限制：不拆多 WorkUnit。多单元是 Phase 2。

### FR-006 内环 WorkerAdapter 启动

- Priority: P0
- Given WorkUnit=`assigned`
- When Supervisor 调用 adapter.start
- Then：
  - 在 inner worktree 中启动
  - cwd=inner worktree
  - 环境变量注入：`WANAX_RUN_ID` `WANAX_WORK_UNIT_ID` `WANAX_TEST_COMMAND`
  - **不注入** 任何 `GIT_ASKPASS` / `GH_TOKEN` / ssh agent 转发（测试断言环境不含这些）
- `octoscode` adapter：以 `--yolo` 或等价非交互标志运行；若该标志不存在，写 `[NEEDS CLARIFICATION]` 到 doctor，不得假装成功
- `fake` adapter：按 fixture 改文件、跑 test_command、写假 receipt

### FR-007 内环回合与 turn 计数

- Priority: P0
- 每次 adapter 报告完成一轮工具循环，`spent_inner_turns += 1`
- `spent_inner_turns >= max_inner_turns` → 发 cancel，state=`budget_exhausted`
- 正向：turns=39 时仍可完成并交回执
- 负向：turns=40 且尚未 receipt → 不 accept

### FR-008 内环回执收集

- Priority: P0
- Worker 退出码 0 或 adapter 声明 done 后，Supervisor 在 inner worktree 执行：
  1. `git add` 仅对 changed tracked/untracked（仍受 forbidden_globs 过滤，命中则不 add 并标 `boundary_violation`）
  2. 若有变更：`git commit -m "wx(<run_id>): <work_unit.title>"`
  3. 收集 Receipt 字段
- 无变更：Receipt.changed_files=[]，后续外环默认 `reject`（除非 instruction 是纯调研且 Phase 1 不支持调研任务）
- 负向：worker 崩溃无 commit → `receipt` 不生成，Run=`failed`，`E_WORKER_CRASH`

### FR-009 内环自称 pass 不作数

- Priority: P0
- Receipt.claimed_pass 只记录，不改变外环决策
- 即使 claimed_pass=true，也必须进入 `outer_reviewing`

### FR-010 外环干净树复测

- Priority: P0
- Given receipt_ready
- When 进入 outer_reviewing
- Then：
  1. `git worktree add` 新目录 outer
  2. `git checkout` inner_commit_sha
  3. 在 outer 树执行 Contract.test_command，timeout=test_timeout_secs
  4. 记录 exit code 与 excerpt
- 禁止：`cd` 到 inner 树跑测试
- 正向：测试 0 → 继续边界检查
- 负向：超时 → 视为 outer_test_exit_code=124，decision 不得 accept

### FR-011 边界检查

- Priority: P0
- 计算 `git diff --name-only base_sha..inner_commit_sha`
- 每个文件必须匹配至少一条 allowed_globs
- 任一文件匹配 forbidden_globs → boundary_ok=false
- `.wanax/runs/**` 与 `.wanax/worktrees/**` 不计入越界
- 正向：只改 `src/foo.rs` 且 allowed=`src/**`
- 负向：顺手改了 `Cargo.toml` 但 allowed 不含 → reject

### FR-012 外环模型审查（非机械部分）

- Priority: P0
- Commander 读取：Contract、diffstat、changed file 列表、outer test excerpt、Receipt
- Commander **不**读取完整 inner transcript
- 输出必须符合 Verdict schema
- 若 outer_test_exit_code!=0，Commander 不得输出 accept；若输出 accept，系统强制改写为 `rework` 并追加 system note `E_ACCEPT_OVERRIDE`

### FR-013 Rework 循环

- Priority: P0
- decision=`rework` → rework_count+=1，写新 instruction（必须包含失败原因与失败测试摘录），state 回 `dispatched`
- rework_count>3 → 强制 `escalate`
- 正向：第一次测试红，第二次绿，accept
- 负向：四次仍红 → escalate，不无限转

### FR-014 accept 前置条件（机械，不可被模型覆盖）

- Priority: P0
- accept 当且仅当：
  1. outer_test_exit_code==0
  2. boundary_ok==true
  3. Receipt.test_command==Contract.test_command
  4. inner_commit_sha 是 base_sha 的后代
  5. changed_files 非空
  6. budget 未耗尽
  7. rework_count≤3
- 缺少任一条 → 系统拒绝把 state 写成 accepted

### FR-015 预算会计

- Priority: P0
- 每次 LLM 调用后根据 provider usage 累加 `spent_usd_micros`
- 若 provider 不返回 usage：用 `chars_in/out * config.estimate_rates` 计入，并在 Tombstone 标 `cost_estimated=true`
- `spent >= max` → 停止新的 LLM 调用，cancel worker，state=`budget_exhausted`
- 默认费率写在 config，不写死在代码常量以外的 fallback：
  - commander: $10 / 1M in, $50 / 1M out（可配）
  - inner: $0.30 / 1M in, $1.20 / 1M out（可配）
- [NEEDS CLARIFICATION] 真实供应商价表以用户配置为准

### FR-016 cancel

- Priority: P0
- `wanax cancel <run_id>`
- 发 SIGTERM 给 worker，`cancel_timeout_secs=20` 后 SIGKILL
- 保留 worktree 与 Tombstone
- 释放 LOCK
- state=`cancelled`

### FR-017 status

- Priority: P0
- `wanax status [run_id]`
- 输出：state、spent_usd、spent_turns、current work unit、last event time、outer test 结果
- 无 Run → exit 7，`E_RUN_NOT_FOUND`

### FR-018 doctor

- Priority: P0
- 检查：git、adapter bin 存在、API key 环境变量存在（不打印 key）、锁是否陈旧、disk 可写
- 缺 key → 警告但不 exit 0 以外；`--strict` 时缺 key exit 8

### FR-019 内环无推送权（安全测试必须覆盖）

- Priority: P0
- 集成测试：fake adapter 尝试 `git push` 必须失败
- inner worktree 的 `remote.url` 可存在，但 credential helper 为空且 `GIT_TERMINAL_PROMPT=0`

### FR-020 P0 产出物

- Priority: P0
- accept 后：
  - 在 repo 主 worktree **不自动 merge**
  - 保留 `wanax/<run_id>/inner` 分支
  - 写 `RESULT.md` 到 run 目录：decision、sha、test excerpt
  - stdout 打印分支名与 `git diff base..inner --stat`
- 合入主线是人类或 P1 GitHub PR 的事

### FR-021 test_command 安全子集

- Priority: P0
- 允许：`cargo test ...`、`npm test`、`pnpm test`、`pytest`、`go test ...`、`make test`
- 拒绝（start 时就失败）：包含 `rm ` `sudo` `mkfs` `dd ` `curl |` `wget |` `>` 写到 `/` 或 `$HOME`
- 实现用正则黑名单 + 按空白 split 后检查 argv[0] 白名单
- 负向：`cargo test && rm -rf /` → `E_TEST_COMMAND_FORBIDDEN`

### FR-022 Goal 循环（Phase 2）

- Priority: P1
- GoalAgent 内部循环最多 `max_goal_iters=8`
- 每轮必须跑 test_command
- 自审模型 ≠ 实现模型；否则自审结果只能是 `mechanical`，Tombstone 标 `self_review=degraded`
- Goal 不能把 degraded self-review 写成 outer accept

### FR-023 Peer 隔离（Phase 3）

- Priority: P1
- 每个 Peer 独立 `git worktree add`
- 禁止两个 Peer 的 allowed 文件集相交；相交则 Commander 必须串行或重拆，否则 `failed` `E_PEER_OVERLAP`
- 回收：peer 完成后 cherry-pick 或 merge 到 inner_branch，冲突 → `blocked`

### FR-024 GitHub PR（Phase 3+）

- Priority: P2
- 仅外环在 accept 后调用 `gh pr create`
- token 只给外环进程，不给 worker 子进程
- [NEEDS CLARIFICATION] 是否默认开启

### FR-025 日志与隐私

- Priority: P0
- 日志默认本地：`.wanax/runs/<id>/wanax.log`
- API key、`Authorization` 头必须 redaction
- 不默认上报遥测

---

## Detailed User Flows

### Flow A — 首次关灯（Phase 1 主路径）

1. 人在已有 git repo 执行 `wanax init`
2. 人编辑 `specs/foo.contract.md`，填四段 + test_command
3. 人执行 `wanax start --contract specs/foo.contract.md`
4. 系统校验契约、锁仓、冻 hash、建分支、写墓碑
5. Commander 生成 1 个 WorkUnit，写入墓碑
6. Supervisor 在 inner worktree 拉起 octoscode/fake
7. 工人改代码、跑测试、退出
8. Supervisor 提交 inner commit，写 Receipt
9. 系统新建 outer worktree，checkout 该 commit，复跑 test_command
10. 系统做边界检查
11. Commander 出 Verdict（受 FR-014 机械约束）
12a. accept → 打印分支与 diffstat，放锁，结束
12b. rework → 回到 5，带失败摘录
12c. reject/escalate/budget → 放锁，结束，保留现场

分支：

- start 时 dirty → 中止
- worker 超时 → failed
- 外环测试超时 → rework 或 escalate（第 3 次起 escalate）

### Flow B — 人中途介入

1. `wanax status` 查看
2. `wanax cancel` 停止
3. 人可直接看 inner 分支，不经过工厂合入

### Flow C — 锁残留

1. 进程被 kill -9
2. `start` 报 E_REPO_LOCKED
3. `wanax doctor` 显示 pid dead
4. `wanax doctor --fix-lock` 清锁
5. 旧 Run 标 `failed`，不自动 resume（P0 不实现 resume；P1 再做）

---

## Screen / Component Inventory & States

v1 无 GUI。组件是 CLI 与文件。

### C-1 CLI `wanax start`

| State | 表现 |
|---|---|
| default | 打印 `run_id` + state 流转一行一条 |
| loading | `starting worker pid=...` |
| empty | 不适用 |
| error | stderr 一行 `ERROR <CODE> <message>`，exit 非 0 |
| disabled | adapter 缺失时 doctor 已警告；start 直接 `E_ADAPTER_MISSING` |

### C-2 CLI `wanax status`

| State | 表现 |
|---|---|
| default | 表格：Run / State / Unit / USD / Turns / LastEvent |
| loading | 读 DB >200ms 仍可阻塞，无需 spinner |
| empty | 无 run：`No runs.` exit 0（与 `status <id>` 找不到的 exit 7 区分） |
| error | DB 损坏：`E_DB` exit 9 |

### C-3 TOMBSTONE.md

| State | 表现 |
|---|---|
| default | 按事件时间倒序或正序（正序），每节含 actor/kind/time |
| loading | 不适用（生成是同步的） |
| empty | 仅有 run_started |
| error | envelope 写失败则 Run=`failed`，不得留下只写了一半的 JSON |

### C-4 RESULT.md

| State | 表现 |
|---|---|
| default | accept/reject 后生成 |
| empty | 非终态不存在该文件 |
| error | 不生成 |

禁止做 TUI 进度条动画、Web UI、系统托盘。

---

## Edge Cases & Error Catalog

| Code | Scenario / Input | Expected Behavior | Message |
|---|---|---|---|
| E_NOT_GIT | 目录无 `.git` | 不写文件，exit 2 | `not a git repository` |
| E_ALREADY_INIT | `.wanax/config.toml` 已存在 | exit 3，除非 `--force` | `already initialized` |
| E_CONTRACT_INVALID | 缺字段 / globs 空 / test 空 | exit 4，不建 Run | `invalid contract: <fields>` |
| E_DIRTY_WORKTREE | 有未提交变更 | exit 5 | `dirty worktree; commit, stash, or pass --allow-dirty` |
| E_REPO_LOCKED | LOCK 存在且 pid 活着 | exit 6 | `repo locked by run <id>` |
| E_RUN_NOT_FOUND | status/cancel 未知 id | exit 7 | `run not found` |
| E_ADAPTER_MISSING | octoscode 不在 PATH | start exit 8 | `adapter binary not found: octoscode` |
| E_MISSING_API_KEY | `--strict` 且无 key | exit 8 | `missing WANAX_COMMANDER_API_KEY` |
| E_DB | sqlite 损坏 | exit 9 | `database error` |
| E_TEST_COMMAND_FORBIDDEN | 危险 test_command | start exit 4 | `test_command rejected` |
| E_WORKER_CRASH | worker 非 0 且无 receipt | state=failed，放锁 | tombstone error event |
| E_WORKER_TIMEOUT | 超过 worker.timeout_secs | 杀进程，failed | `worker timeout` |
| E_OUTER_TEST_TIMEOUT | 复测超时 | 不得 accept | exit 124 记入 verdict |
| E_BOUNDARY | 文件越界 | boundary_ok=false，reject 或 rework | 列出越界路径 |
| E_ACCEPT_OVERRIDE | 模型想 accept 但测试红 | 强制 rework | system note |
| E_COMMANDER_SCHEMA | 外环 JSON 非法 | 重试 2 次后 failed | `commander schema invalid` |
| E_BUDGET | 超 USD 或 turns | budget_exhausted | `budget exhausted usd=<x> turns=<y>` |
| E_REWORK_LIMIT | rework>3 | escalate | `max rework exceeded` |
| E_CONTRACT_MUTATED | 运行中契约文件被改 | 忽略磁盘新内容，沿用冻结 hash；status 警告 | `contract mutated on disk; run still uses frozen hash` |
| E_CONTRACT_TESTS_WRITABLE | `allowed_globs` 能改到 `tests/` | start/doctor 警告，不阻断；`doctor --strict` exit 4 | `allowed_globs include binding tests; a worker can rewrite them` |
| E_PUSH_ATTEMPT | 内环 git push | 失败；若意外成功视为安全漏洞测试失败 | push denied |
| E_PEER_OVERLAP | P1 文件集相交 | failed，不合并 | `peer file sets overlap` |
| E_LOCK_STALE | pid 死锁文件在 | start 仍拒绝；doctor --fix-lock 清 | `stale lock pid=<n>` |
| E_PROTECTED_REF | 内环试图 checkout main | adapter 包装层拒绝 | `protected ref` |

所有错误码稳定，写入 `src/error.rs` 枚举，禁止同一语义多个字符串。

---

## Non-Functional Requirements

| ID | Requirement | 度量 |
|---|---|---|
| NFR-1 | Phase 1 控制面启动开销 | `wanax status` 在 10k event 的 DB 上 p95 < 200ms |
| NFR-2 | Tombstone 追加 | 单次 event append + fsync p95 < 50ms（本地 SSD） |
| NFR-3 | 外环复测隔离 | 100% 用例不在 inner cwd 执行验收测试 |
| NFR-4 | 密钥泄漏 | 单测扫描 log/tombstone fixture，禁止出现 `sk-` `ghp_` 明文 |
| NFR-5 | 崩溃一致性 | kill -9 后 DB 可打开，Run 处于合法状态或可被 doctor 标 failed |
| NFR-6 | 二进制体积 | release 默认 features 不含 web；目标 < 20MB（不含静态 LLM） |
| NFR-7 | OS | CI：Ubuntu 24.04 + macOS aarch64。Windows 不作为 P0 |
| NFR-8 | Rust | 1.85+，`cargo test` 全绿，clippy -D warnings |
| NFR-9 | 并发 | P0 单 Run / repo。内部 tokio，worker 为子进程 |
| NFR-10 | i18n | CLI 信息默认英文（便于日志搜索）；`--lang zh` 可选，P2。契约文件本身支持中英标题 |
| NFR-11 | 合规 | v1 本地单用户，无云端 PII 收集。不实现 GDPR 删除接口，因为没有账号系统 |
| NFR-12 | 可重复构建 | `cargo build --locked` |

---

## Technical Constraints, Assumptions & Preferred Stack

### Stack（强制，除非用户改口）

| Layer | Choice |
|---|---|
| Language | Rust 2021 / 2024 edition，workspace |
| CLI | `clap` derive |
| Async | `tokio` + `tokio::process` |
| DB | `sqlx` + SQLite |
| Git | `git` CLI 包装，不在 P0 引入 `git2` 复杂索引操作；worktree/add/diff/commit 全走 git 子进程 |
| Serialize | `serde` + `serde_json` + `toml` |
| Hash | `sha2` |
| ID | `ulid` |
| Lock | `fs2` 或 `fd-lock` |
| HTTP/LLM | `reqwest`；OpenAI-compatible + Anthropic Messages 两个 provider |
| Test | `cargo test` + 本地 git fixture repos |
| Logging | `tracing` + 文件 rolling 可选，P0 单文件即可 |

### Crate 切分（建议）

```
wanax/
  crates/wanax-cli
  crates/wanax-core      # state machine, types
  crates/wanax-tombstone
  crates/wanax-git
  crates/wanax-llm
  crates/wanax-worker    # adapters
  crates/wanax-verify
```

禁止把 adapter、LLM、git 全塞进一个 `main.rs`。

### Assumptions

1. 机器上已安装 `git`。
2. 目标项目有可脚本化测试。没有测试的仓库，工厂应当拒绝，而不是用 LLM “感觉对”。
3. 用户能提供至少一个贵模型 key 和一个便宜模型 key（可以是同一个 provider 不同 model）。
4. OctoLoop 即将开源不构成法律障碍：蚁后是独立控制面，不复制 octoscode 源码。
5. [NEEDS CLARIFICATION] 用户是要个人自用还是要发布 crates.io。默认按可发布 OSS CLI 写，但不做官网。
6. [NEEDS CLARIFICATION] 第一适配器是否必须是 octoscode。若本机没有，Phase 1 可用 fake + 任意 `cmd` adapter（用户指定二进制与参数模板）。

### Preferred worker command template

```toml
[worker]
adapter = "octoscode"
octoscode_bin = "octoscode"
# Phase 1 允许通用 cmd adapter：
# adapter = "cmd"
# argv = ["octoscode", "--yolo", "--message", "{instruction}"]
```

`{instruction}` `{repo}` `{test_command}` 由系统替换。instruction 文件先落到 `WORK_UNIT.md`，避免超长 argv。

---

## Phased Implementation Plan

### Phase 1 — Foundation + 单工人闭环（P0）

范围：FR-001..021、025；NFR 基础；fake adapter 全覆盖；octoscode adapter 能拉起即可。

包含 FR：001–021，025

完成标准：

- `cargo test` 全绿
- 用 fixture 仓库：Contract 要求新增函数 + 单测；fake worker 改代码；外环复测绿；state=accepted
- 对应负向：越界改文件 → reject；测试红 → rework 然后 escalate
- 安全测试：inner 环境无 token，push 失败

验证方式：CI + 一个 `tests/e2e_fake_factory.rs`

### Phase 2 — Commander 真模型 + Goal 循环 + 成本

范围：FR-022，真实 LLM provider，预算真实 usage

完成标准：对一个真实小仓库，用便宜模型改代码、贵模型只出 verdict，Tombstone 完整

验证方式：手工 dogfood + 录制 cassette（`wiremock` 或保存 HTTP fixture），CI 不打真实付费 API

### Phase 3 — Peer worktree + 可选 PR

范围：FR-023、024

完成标准：两个不相交模块并行，回收无冲突；冲突路径 blocked

### Phase 4 — Polish

- `--lang zh`
- `resume` 崩溃恢复
- agent-spec `lifecycle` 作为可选 verifier plugin
- 多 WorkUnit DAG

每个 Phase 结束必须更新 Verification Checklist，未勾选不得开始下一 Phase。

---

## Analytics & Tracking Requirements

v1 不做产品分析埋点。

唯一“分析”是本地 Run 级会计：

- `spent_usd_micros`
- `spent_inner_turns`
- `outer_test_exit_code`
- `rework_count`
- `duration_ms`

禁止默认联网上报。若未来加 telemetry，必须 opt-in 且不含源码与密钥。

---

## Risks, Assumptions & Open Questions

### 风险（先看这些再写代码）

1. **抄 OctoLoop 当产品：这行不通。** 原作者已提交 PR，下周可玩。你从零再造一个 peer/goal TUI，会浪费一个季度。差异化只能在控制面：契约门、墓碑协议、外环机械复测、工人可替换。
2. **没有契约质量，关灯就是把垃圾生产自动化。** 工厂会放大错误。P0 必须能拒绝无测试、无边界的任务。
3. **便宜模型伪造测试输出。** 所以外环必须换树重跑，而不是读内环粘贴的 “All tests passed”。
4. **worktree 与脏仓库。** 用户日常仓库很脏。P0 默认拒绝 dirty，否则 base_sha 无意义。
5. **octoscode 非交互接口变更。** adapter 必须版本探测，失败要明确，不能卡死。
6. **成本失控。** 外环模型贵。Commander prompt 必须短：只给 diffstat + excerpt + 契约，不给整仓。
7. **误合主线。** P0 禁止自动 merge。谁把 auto-merge 当默认，谁在制造事故。
8. **法律/许可。** 不要 vendoring octos / octoscode 源码。只调二进制。
9. **名字混用。** 中文产品名是蚁后，品牌词是 wanax，Commander 是角色名。禁止再引入 NightForge / antqueen。禁止把 Tombstone/Worker 改成信息素/工蚁类型名。海外分发只用 Wanax，不要再发明第三个品牌。
10. **品牌未做商标检索。** 2026-08-30 仅确认 crates.io 与 npm 精确包名 `wanax` 为 404。域名、GitHub org、各国商标未查。发布前必须自己做一遍。

### 开放问题

- [NEEDS CLARIFICATION] 你要的是个人控制面，还是一个打算让别人装的开源产品？两者的 CLI 完成度差一个数量级。
- [NEEDS CLARIFICATION] 第一工人必须是 octoscode，还是 `cmd` 通用适配器优先（更不容易卡在别人的 CLI 上）？
- [NEEDS CLARIFICATION] 目标仓库是不是主要 Rust？若是，Phase 1 可把 `cargo test` + clippy 做成默认 verifier 插件。
- [NEEDS CLARIFICATION] 是否直接依赖 `agent-spec` crate 做 L1–L3，还是 v1 只复用其契约四段结构、自己做 glob+test 两门？
- [NEEDS CLARIFICATION] 外环模型与内环模型的具体供应商与型号。
- [NEEDS CLARIFICATION] 要不要 GitHub Issue 驱动（Orbi 模式）。默认不要，那是另一条产品线。

---

## Verification Checklist

### Phase 1

- [x] `wanax init` 在 git repo 生成约定文件
- [x] 非 git 目录 init 失败且无副作用
- [x] 无效契约 start 失败，无 LOCK
- [x] 有效契约 start 产生 run_id、冻结 sha256、两条分支名
- [x] 同一 repo 第二 start 得到 E_REPO_LOCKED
- [x] fake worker 改 allowed 文件、提交 inner commit
- [x] 外环在不同 worktree 复跑测试（断言 cwd ≠ inner）
- [x] 测试红时不能 accepted
- [x] 越界文件 boundary_ok=false
- [x] claimed_pass=true 但外环测试红 → 非 accept
- [x] 模型输出 accept 但测试红 → E_ACCEPT_OVERRIDE
- [x] turns 或 usd 超限 → budget_exhausted
- [x] cancel 杀子进程并放锁
- [x] 内环环境无 GH_TOKEN / ssh 转发
- [x] tombstone envelope.json 可还原全部 event
- [x] 危险 test_command 被拒绝
- [x] clippy -D warnings 通过
- [x] 不存在 Web UI / TUI / telemetry 代码

### Phase 2

- [x] 真实 LLM fixture 可跑通一次 accept
- [x] 自审模型与实现模型相同 → self_review=degraded
- [x] 成本估算或真实 usage 写入墓碑

### Phase 3

- [ ] 相交文件集的两个 peer 被拒绝
- [ ] 不相交 peer 结果回收到 inner_branch
- [ ] PR 创建仅发生在 accept 之后且仅外环持有 token

---

## Appendix A — Contract 文件格式（P0 必须能 parse）

```markdown
---
spec: wanax.contract
version: 1
name: "split-timeout-module"
test_command: "cargo test -p foo --timeout-mod"
test_timeout_secs: 180
allowed_globs:
  - "crates/foo/src/**"
  - "crates/foo/tests/**"
forbidden_globs:
  - "**/.env"
  - "crates/foo/src/lib.rs"
---

## Intent

把超时逻辑从 `handler.rs` 抽到独立模块，行为保持不变。

## Decisions

- 新模块路径：`crates/foo/src/timeout.rs`
- 不引入新依赖

## Boundaries

- 允许：`crates/foo/src/**`、`crates/foo/tests/**`
- 禁止：改 `lib.rs` 以外的对外 crate API 形状（机械门只禁 `lib.rs` 若列入 forbidden；API 形状由测试保证）

## Completion Criteria

- CC-01: `cargo test -p foo` 退出码 0
- CC-02: 存在 `crates/foo/src/timeout.rs`
- CC-03: 原有超时单测仍通过（bound_test: `timeout_expires_returns_error`）
```

Front matter 与标题都要能解析。中文标题别名：

| English | 中文别名 |
|---|---|
| Intent | 意图 |
| Decisions | 已定决策 |
| Boundaries | 边界 |
| Completion Criteria | 完成条件 |

### Appendix B — envelope.json 最小例子

```json
{
  "schema_version": "1.0.0",
  "run_id": "wx_01K3EXAMPLE00000000000000",
  "contract_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
  "current_state": "inner_working",
  "events": [
    {
      "id": "wx_01K3EVENT0000000000000000",
      "at": "2026-08-31T00:00:00Z",
      "actor": "system",
      "kind": "run_started",
      "payload": {"base_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
      "payload_sha256": "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
    }
  ]
}
```

示例里的 hash 是占位符，实现时必须按真实内容计算。`payload_sha256` 为 canonical JSON（object key 按 UTF-8 字节序排序、无多余空白）的 SHA-256 hex。

### Appendix C — CLI 摘要

```
wanax init [--force]
wanax start --contract <path> [--allow-dirty] [--adapter octoscode|fake]
wanax status [run_id]
wanax cancel <run_id>
wanax doctor [--fix-lock] [--strict]
wanax verdict <run_id>          # 只读打印最近 Verdict，P0 不提供人工覆盖 accept
```

P0 **禁止** `wanax accept` 人工强行盖章绕过测试。人要合入就自己 git merge 分支。

### Appendix D — 命名映射（禁止混用）

| 用途 | 必须用 |
|---|---|
| 产品中文名 | 蚁后 |
| 产品英文名 | Wanax |
| crate / binary / CLI | `wanax` |
| 仓库目录 | `.wanax/` |
| 用户数据目录 | `~/.wanax/` |
| Run / Event ID | `wx_` + ULID |
| 环境变量 | `WANAX_COMMANDER_API_KEY` `WANAX_INNER_API_KEY` |
| git 分支 | `wanax/<run_id>/inner` `wanax/<run_id>/outer` |
| 契约 front matter | `spec: wanax.contract` |
| 废弃名 | NightForge / nightforge / nf_ / NIGHTFORGE_* / antqueen / Ant Queen / aq_ / ANTQUEEN_* |

隐喻到此为止。不要把 Tombstone 改名为信息素，不要把工厂法改名为蚁群法则，不要把 Worker 类型改名为 WorkerAnt。

### Appendix E — 对原帖的映射（防止实现者理解跑偏）

| 原帖概念 | 蚁后 |
|---|---|
| 关灯工厂 | FactoryRun + 无人值守直到终态 |
| OctoLoop 内环苦力班 | WorkerAdapter + Master/Goal/Peer |
| 外环 Claude/Codex/Fable5 | Commander + 独立复测 |
| 墓碑 markdown | Tombstone envelope.json + 渲染 md |
| goal peer agent | Phase 2/3，不是 Phase 1 |
| agent-spec SDD | Contract 四段 + 机械验证门 |
| Omarchy 多窗口 | 非目标。工厂不依赖桌面 |
| 自举开发 | 允许用蚁后开发蚁后，但不作为 P0 功能 |

---

## 最终自检

- [x] 完整 Data Model 表格
- [x] 核心 FR 含 Given/When/Then + 正负向例子
- [x] 独立 Edge Cases & Error Catalog
- [x] 清晰 Phased Implementation Plan
- [x] 明确 Non-Goals 与“不得自行添加功能”
- [x] 去掉无度量的“简单/高性能/优雅”等词
- [x] How to Use with Agents 完整
- [x] 开放问题用 [NEEDS CLARIFICATION] 标记
- [x] Agent 不得实现未要求功能的声明已写在 How to Use 第 6 条
