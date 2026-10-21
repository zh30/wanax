# Wanax v1.1 Hardening & Improvement Plan

| 字段 | 值 |
|---|---|
| Document Title | Wanax Hardening & Improvement Backlog |
| Version | 1.1.0 |
| Date | 2026-10-20 |
| Status | Implemented — all items landed |
| Base | v1 / Phase 6 complete (`docs/PRD.md`) |

本文档是 v1.1 的开发待办。核心判断：v1 的架构方向正确（契约门 + 机械外环复测 + 墓碑协议 + 可替换工人），但 **"五条不可违反的工厂法"在实现上是约定而非机制**——隔离靠 env 剥离与 PATH 垫片，预算不覆盖真正花钱的内环，spec 承诺的并行没有发生。

改动原则：保持本地单用户前台 CLI 定位；不引入 daemon；worker/test/commander 任一子进程的隔离与计量统一走 wanax-core 原语。

---

## Phase H — 安全硬边界（Security Hardening）

> ✅ H-1 ~ H-4 已全部实现并测试。新增模块：`wanax_core::envenv`（白名单 env）、`wanax_core::sandbox`（Seatbelt/bwrap）；配置 `[test] pass_env`、`[verify] baseline_test`、`[sandbox]`；`wanax doctor` 报告 sandbox backend；`run_started` tombstone 事件记录 `sandbox` 字段。

### H-1 统一子进程环境清洗

**问题**：`sanitized_env` 只用于 worker 进程。`run_test_command`（`sh -c`）继承 wanax 全部环境——worker 写的测试代码可读到 `WANAX_COMMANDER_API_KEY`，打印后进 excerpt → tombstone → commander prompt → LLM API。`CliCommander` spawn 不 `env_clear`，`claude -p --dangerously-skip-permissions` 带全部父环境（`GH_TOKEN`、SSH agent、两个 WANAX key）。

**方案**：

- `wanax-core::envenv`：`base_child_env()` 白名单（`PATH HOME USER TMPDIR LANG LC_ALL TERM SHELL`）+ `FORBIDDEN_ENV` 拒绝表（`GIT_ASKPASS GH_TOKEN GITHUB_TOKEN SSH_AUTH_SOCK SSH_AGENT_PID WANAX_COMMANDER_API_KEY WANAX_INNER_API_KEY`）+ `pass_env(keys)` 显式放行。
- `run_test_command` 签名加 `env`，engine 用 `envenv` 构造；新配置 `[test] pass_env = []` 允许契约需要的环境变量透传。
- `CliCommander::complete` 加 `env_clear + base_child_env`。
- `wanax-worker::sanitized_env` 改为调用 `envenv`，保持公开 API 不变。

**验证**：单测断言 forbidden key 不进 env；e2e：worker 测试打印 `$WANAX_COMMANDER_API_KEY` 到文件，文件内容为空。

### H-2 gitconfig 进程级隔离（不再改用户 repo config）

**问题**：`harden_inner_worktree` 在 linked worktree 里跑 `git config --local`。未开 `extensions.worktreeConfig` 时 `--local` 写的是**共享** `.git/config`——每次 run 把 `credential.helper ""`、`commit.gpgsign false` 永久写进用户主仓配置（已实测）。

**方案**：

- `harden_inner_worktree` 删除 `--local` 写；改为 `prepare_worker_gitconfig(repo)` 生成 `<repo>/.wanax/gitconfig`：
  ```ini
  [credential]
      helper =
  [commit]
      gpgsign = false
  [user]
      name = wanax
      email = wanax@local
  ```
- worker/test/commander 子进程 env 设 `GIT_CONFIG_GLOBAL=<该文件>` + `GIT_CONFIG_NOSYSTEM=1`。
- `.wanax/gitconfig` 加进 init 的 `.wanax/.gitignore`。

**验证**：单测断言 run 后 repo `.git/config` 无 `credential.helper`；`GIT_CONFIG_GLOBAL` 出现在子进程 env。

### H-3 进程组 kill

**问题**：`run_test_command` 超时 `child.kill()` 只杀 `sh`，`cargo test` 等孙进程继续写 worktree；adapter `cancel` 只 TERM 主 pid，工具子进程成孤儿。

**方案**：

- Unix 下所有 spawn 加 `process_group(0)`（新进程组，leader pid = pgid）。
- `kill_tree(pid)` 原语：`kill -TERM -- -pgid`，超时路径直接 `kill -KILL -- -pgid`。
- `run_test_command` 超时 → KILL 整组；adapter `cancel` → TERM 整组；`start_stdin_cli`/`OctoscodeAdapter` 超时 → KILL 整组。

**验证**：`sh -c 'sleep 60' ` 超时后 `kill -0 <grandchild>` 失败。

### H-4 OS 沙箱层（worker / test / commander CLI）

**问题**：skip-permissions worker 拥有完整 FS 写 + 网络。`allowed_globs` 只约束 commit，不约束行为。git wrapper 是 PATH 垫片，worker 直调 `/usr/bin/git` 即绕过。

**方案**：`wanax-core::sandbox`

```toml
[sandbox]
mode = "auto"            # auto | off | seatbelt | bwrap
extra_write_paths = []   # 额外可写目录（用户工具链缓存等）
```

- `auto`：macOS 有 `sandbox-exec` → Seatbelt；Linux 有 `bwrap` → bwrap；否则 unsandboxed + tombstone `sandbox=none` 警告。
- Seatbelt profile（生成到 `<repo>/.wanax/runs/<id>/sandbox-<role>.sb`）：
  ```
  (allow default)
  (deny file-read* (subpath ~/.ssh) (~/.aws) (~/.gnupg) (~/.config/gh) (~/.kube) (~/.docker))
  (deny file-write*)
  (allow file-write* (subpath worktree) (subpath <repo>/.git) (subpath run_dir)
        (subpath TMPDIR) /tmp /private/tmp /dev/null /dev/tty
        ~/.cargo ~/.rustup ~/.cache ~/Library/Caches ~/.npm ~/.pnpm-store ~/go/pkg)
  ```
  允许写 `<repo>/.git` 是必需的：linked worktree 的 git 元数据在主仓 `.git/worktrees/` 下。
- bwrap：`--ro-bind / /` + worktree/repo.git/caches `--bind` + secret dirs `--tmpfs` 遮盖 + `--proc /proc --dev /dev --die-with-parent`。不 unshare-net（worker 需要 API 网络）。
- `SandboxSpec` 进 `WorkerContext`；`run_test_command` 同样包裹；`CliCommander` 以 scratch 目录为可写域。
- `wanax doctor` 输出 resolved sandbox mode；`mode=auto` 无可用后端 → WARN。

**验证**：单测断言生成的 argv/profile；e2e（后端存在时）：在沙箱里 `touch ~/.zshrc.wanax-test` → 失败，写 worktree 文件 → 成功。

---

## Phase I — 正确性与可观测性

> ✅ I-1 ~ I-4 已全部实现并测试。`WorkerHandle` 新增 `cost_usd_micros`/`usage_in`/`usage_out`；`run_peer_batch` 三阶段化（串行准备 → JoinSet 并发 → 串行落账）；`CompletionClient::complete` 新增 `json_mode` 参数。

### I-1 内环真实计量

**问题**：所有 adapter 返回 `turns=1`、`$0`。`claude -p` 内部烧的钱不进 `spent_usd_micros`——预算只记 commander + self-review。

**方案**：

- `ClaudeAdapter` argv 追加 `--output-format json`（用户 `claude_args` 已含时跳过）；解析 `total_cost_usd` → `cost_usd_micros`、`num_turns` → `turns`、`is_error` → crash、`result` → excerpt。
- `CodexAdapter` argv 追加 `--json`（同上跳过）；逐行 JSONL 解析 `turn.completed` 的 `usage.input/output_tokens`。
- `WorkerHandle` 加 `cost_usd_micros: Option<i64>`、`usage_in/out: Option<u64>`；`budget::charge_usd_micros` 直接记 USD，token 路径走 `charge_units(inner rates)`；goal loop 与 peer batch 都在 `add_turns` 后记账并写 `budget_tick`。

**验证**：假 `claude` 输出 `{"total_cost_usd":0.5,"num_turns":3}` → `spent_usd_micros=500_000`、`spent_inner_turns=3`；假 `codex --json` 输出 `turn.completed` usage → 按 inner rate 折算。

### I-2 baseline 测试

**问题**：`start` 不在 `base_sha` 上跑 `test_command`。契约烂要等 worker 烧一轮才 fail；基线已绿的契约存在 vacuous-green accept 风险（trivial diff + 原本就绿的测试 → 机械门全过）。

**方案**：

- 配置 `[verify] baseline_test = true`（默认开）。
- `run_factory` 在 inner_wt（此刻内容 == base_sha）派活前先跑 `test_command`，写 `baseline_test` tombstone 事件（exit_code、excerpt）；失败不阻断 run，verdict prompt 增加 `baseline_test_exit_code=`。
- `VerdictContext` 加 `baseline_test_exit_code: Option<i32>`；resume 时从 envelope 事件恢复。

**验证**：e2e 断言 envelope 含 `baseline_test` 事件且 exit_code 为红（fake fixture 基线缺函数）；`baseline_test=false` 时无事件。

### I-3 peer batch 真并行

**问题**：`run_peer_batch` for 循环逐个 `await`，spec 的不相交并行没有发生（DAG 节点共享 inner worktree，本轮不做并行——需要 peer 式独立 worktree，记入 backlog）。

**方案**：per-peer 准备工作（branch/worktree/unit/WORK_UNIT.md）保持串行，`start_adapter` 用 `tokio::task::JoinSet` 并发，收据/commit/event 按 seq 顺序串行落账。

**验证**：两个 `sleep_ms=1500` 的 fake peer，`run_factory` 内环阶段 < 2.5s（串行基线 ≥3s）。

### I-4 commander 结构化输出

**问题**："Reply with JSON only" + 正则抽取 + 3× 重试，`E_COMMANDER_SCHEMA` 是一整个可消除的错误类。

**方案**：

- `CompletionClient::complete` 加 `json_mode: bool` 参数（或 `schema: Option<&Value>`）。
- openai/openai_compat：`"response_format": {"type": "json_object"}`（OpenRouter/兼容端点普遍支持）。
- anthropic：`tools=[{name:"emit",input_schema:{"type":"object"}}]` + `tool_choice:{type:"tool",name:"emit"}`；`parse_anthropic_body` 识别 `tool_use` block → `input` 序列化为 text。
- `claude_cli` 默认 args 追加 `--output-format json`，stdout 为 `{"result":...,"is_error":...}` envelope，解析 `result` 后再走 `extract_json_object`。
- 保留现有 schema 校验与重试兜底。

**验证**：provider 单测断言 request body 含 `response_format` / `tools`；anthropic `tool_use` body 解析；cli envelope 解析。

---

## Phase R — RSI 递归自我改进（v1.2）

**定义**：递归自我改进在 Wanax 语境 = *测量 → 证据回路 → 有界迭代*，机械门永不被绕过。契约在 run 生命周期内依旧冻结——RSI 迭代的是 dispatch 策略，不是 spec。

| # | 项目 | 内容 | 状态 |
|---|------|------|------|
| R-1 | `wanax report` | envelope → fitness：终态、rework、成本、耗时、错误码分布；单 run 详情 + 全仓聚合视图 | ✅ 已实现+测试 |
| R-2 | `wanax improve` | RSI 循环：每次迭代 = 完整新 run（新 run_id、`parent_run_id` 链接、`iteration` 记 tombstone）；上一轮失败证据（outer exit、越界文件、verdict reason、错误码）打包注入下一轮 dispatch；accepted 或迭代上限为止 | ✅ 已实现+测试 |
| R-3 | `wanax clean` | 清掉已终结且未 accepted 的 run 的 worktree 与 `wanax/<id>/*` 分支；`--dry-run` 预览、`--include-accepted`、`--runs` 连带 run 目录 | ✅ 已实现+测试 |
| R-4 | 自我契约 | `specs/wanax-self.contract.md`：wanax 在自己身上跑（test_command = `cargo test --workspace`） | ✅ 已交付 |
| R-5 | 跨 run 记忆 | `.wanax/LEARNINGS.md`：每个 run 结束追加一行蒸馏（state/verdicts/outer_exits/errors/sandbox）；dispatch 时注入尾部 20 行 —— 记忆跨契约、跨 run 生效，不止 improve 链内 | ✅ 已实现+测试 |
| R-6 | 无进展早停 | improve 比较相邻迭代证据签名（终态+verdict 决策序列+exit 码+越界+错误码，剔除 run_id/路径噪声）；签名相同 → 提前停，不烧满迭代 | ✅ 已实现+测试 |
| R-7 | dogfood 暴露的修复 | CliCommander 非零退出只带 stderr → claude 的 `is_error` envelope 在 stdout 上被丢弃（"Not logged in" 不可见）。现先试解 stdout envelope，再回退 stderr+stdout 尾部 | ✅ 已修复 |
| R-8 | 跨调用迭代链 | `improve --from <run_id>`：用任一已终结 run 的证据做种子（强制同契约 sha）——迭代链不再局限单次命令调用 | ✅ 已实现+测试 |
| R-9 | 策略变异 | `improve --adapters a,b,c`：迭代 i 用 adapters[i-1]（钳位）——便宜 worker 先试，失败升级；preflight 整个梯子 | ✅ 已实现+测试 |
| R-10 | 契约维度 fitness | `wanax report` 聚合输出按契约分组的 accept 率——回答 RSI 的核心问题"这份 spec 在收敛吗" | ✅ 已实现 |
| R-11 | 循环级预算 | `improve --max-total-usd`：跨全部迭代的花费硬顶（per-run 预算管不了 N 轮总账） | ✅ 已实现+测试 |
| R-12 | 崩溃孤儿回收 | `clean --include-dead`：非终态但 owner pid 已死的 run 可清理；活进程永远不动 | ✅ 已实现+测试 |
| R-13 | 可审计增强 | verdict 事件记 `baseline_exit` → report 标 `baseline_green`（空绿识别）；`report <id>` 回溯 `parent_run_id` 链；`report --json` 机器可读 fitness | ✅ 已实现 |
| R-14 | DAG 波形并行 | 就绪 dag 单元中 allowed_globs 两两不相交的最大子集组成 wave → 复用 peer 批处理并发执行 + 一次外环复测共享 accept。补上 PRD "不相交模块并行" 的最后缺口 | ✅ 已实现+测试 |
| R-15 | wave rework 幂等 | wave 重跑时：peer branch `git branch -f` 重置、残留 worktree 先清、inner 树回滚到 wave 合并前 sha——修了 peer 批次从未测过的 rework 碰撞 | ✅ 已实现+测试 |
| R-16 | wave 级 rework 账本 | 每次尝试重建 master unit → 自身 rework_count 不归零。新增跨轮 `wave_rework` 账本 + 成员 unit 记账，守住了 MAX_REWORK 上限和 verdict 递进 | ✅ 已实现+测试 |
| R-17 | `report --suggest` | 规则化分诊：boundary 复发→契约太紧；空绿 accept→验收标准真空；budget→拆分；commander_schema→换结构化 provider；重 rework 零 accept→intent 歧义 | ✅ 已实现+测试 |
| R-18 | bound_test 机械化 | 契约 `bound_test:` 注解从死字段变成机械门：cargo 先 `-- --list` 验证测试存在（防 0-match 假绿），再 `-- --exact` 跑绿；失败→rework | ✅ 已实现+测试 |
| R-19 | LEARNINGS 生命周期层 | `## lifetime` 头由全部 entry 重算（runs/accepts/top_errors/契约维度）——长寿命统计不随 20 行注入窗口滑出 | ✅ 已实现+测试 |
| R-20 | 契约趋势序列 | `report` 每契约输出时序终态 `[AEFFA]`——收敛方向一眼可见 | ✅ 已实现+测试 |
| R-21 | 事件溯源 resume | `unit_seeded`/`unit_dispatched`/`receipt_submitted`/`verdict` 携带重建所需全部字段；resume 以 envelope 重放为准，DB 行缺失/分歧时自愈并记 WARN。旧 envelope 缺字段 → 自动回退 DB 路径 | ✅ 已实现+测试 |
| R-22 | egress 白名单代理 | `[worker] egress = "allowlist"`：worker/test 子进程注入 HTTP(S)_PROXY → 本地 CONNECT 代理只放行白名单域名（base_url host 自动并入），Seatbelt 再 `deny network-outbound` 只留 `localhost:proxy`——绕过代理的直连连 DNS 都不通。`"isolated"` = 完全断网（bwrap 用 `--unshare-net`）。补齐文件沙箱之外的最后一层 | ✅ 已实现+测试 |

**RSI 不变量**：
- 契约 sha256 冻结规则不变——跨迭代变化的只有 evidence/notes，它们走 `DispatchContext.rsi_notes`，不进契约文件
- 每轮迭代仍然独立 worktree、独立 outer 复测、`E_ACCEPT_OVERRIDE` 照常生效
- 迭代上限硬编码（默认 3，最多 8），USD/turn 预算每轮独立——fitness 报告让人判断"这轮有没有变好"

**RSI 增强的 backlog**（Phase R 后续）：commander 侧把证据提炼成摘要（当前是机械拼接）、跨 repo 的学习文件（`.wanax/LEARNINGS.md`）、fitness 回归检测（连续 N 轮 fitness 下降自动停）。

## Backlog（本轮不做）

- **并行 DAG**：需要为每个就绪节点分配独立 worktree + 回收合并（= peer 机制复用）。
- **ACP adapter 后端**（claude-code-acp / codex-acp）：一个协议替代 per-CLI flag 矩阵；代价是引入 session 有状态性。
- **OpenAI Responses API provider**：reasoning 系模型主端点；当前 `chat/completions` 仍可用。
- **Event-sourced resume**：envelope.json 直接 replay 恢复状态机，SQLite 降为索引。
- **网络层出向白名单代理**：Seatbelt/bwrap 只能全放或全断，域名级白名单需要本地 egress proxy。

## 不变量（不得在本轮破坏）

- P0 禁止 `wanax accept` 人工盖章；不自动 merge；不 push 主线。
- 内环不得持有 `GH_TOKEN`/`WANAX_*_API_KEY`/`SSH_AUTH_SOCK`。
- 外层复测必须在与内环不同的 worktree。
- tombstone `envelope.json` 为 append-only 真源，md 只读渲染。
- `cargo test --workspace` + `clippy -D warnings` 全绿。
