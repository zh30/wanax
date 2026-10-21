# Wanax

Local, single-user **lights-off software factory**. You write a task contract; Wanax dispatches work, runs it in isolation, retests independently, and leaves an auditable verdict. The Chinese product name is 蚁后. The crate, CLI, and brand token are `wanax`.

**Spec in → mechanical verification → local branch out. You can leave the keyboard. You cannot leave the contract.**

Wanax is a control plane, not another coding agent. Inner workers (cheap models + an existing CLI agent) write code. The outer commander (an expensive model) only dispatches, retests, and stamps a verdict. The two loops talk only through a tombstone. Protected branches stay untouched.

## What it is not

- A chat UI, TUI, or web dashboard
- A hosted SaaS or multi-tenant service
- An agent that merges to `main` or deploys to production
- A replacement for a human-reviewed contract

v1 is a foreground CLI. There is no daemon. Unattended runs are `tmux` / `systemd --user` around `wanax start`.

## How a run works

```
human writes specs/*.contract.md
        │
        ▼
wanax start --contract …
        │
   ┌────┴────┐
   │  outer  │  commander: dispatch + verdict (no repo writes except tombstone)
   └────┬────┘
        │ work unit
        ▼
   ┌─────────┐
   │  inner  │  worker adapter in a git worktree (no push, no protected refs)
   └────┬────┘
        │ receipt
        ▼
   outer retest in a *new* worktree + boundary check
        │
        ▼
   accept | reject | rework | escalate
        │
        ▼
   wanax/<run_id>/inner   +   .wanax/runs/<id>/TOMBSTONE.md
```

Accept requires all of: outer `test_command` exit 0, files inside `allowed_globs`, a non-empty descendant commit, and budget remaining. A commander that says `accept` while tests are red is forced to rework (`E_ACCEPT_OVERRIDE`).

## Requirements

- Linux or macOS (Windows is not a v1 target)
- [Rust](https://rustup.rs/) 1.85+ (`rust-toolchain.toml` pins 1.97.1 for CI)
- Git
- A commander: HTTP API key (OpenAI-compatible or Anthropic), **or** `commander.provider = claude_cli|codex_cli` with that CLI on `PATH`
- A worker CLI with a non-interactive flag (`octoscode --yolo`), **or** `--adapter fake` for tests

## Install

```bash
git clone https://github.com/zh30/wanax.git
cd wanax
cargo install --path crates/wanax-cli
```

From a checkout without installing:

```bash
cargo run -p wanax-cli -- --help
```

## Quick start

One-time machine setup:

```bash
export WANAX_COMMANDER_API_KEY='…'   # HTTP commander only; skip when using claude_cli / codex_cli
export WANAX_INNER_API_KEY='…'       # only if you set a distinct reviewer.model
```

In a **clean** git repo (uncommitted files outside `.wanax/` block `start`):

```bash
wanax init
# edit specs/example.contract.md — this is the real work
git add specs .wanax/config.toml
git commit -m "Add Wanax contract"

# point commander/inner at your provider (once)
# then:
wanax doctor --strict
wanax start --contract specs/example.contract.md
wanax status
```

On accept, stdout prints `wanax/<run_id>/inner` and a diffstat. Merge is yours. The factory does not push and does not open a PR.

## Task contracts

Per-run input is a Markdown file with YAML front matter. `wanax init` writes `specs/example.contract.md`. A contract must include:

| Field | Role |
|---|---|
| `test_command` | Bound test; run again on a clean outer worktree |
| `allowed_globs` | Only these paths may change |
| Intent | What to do |
| Decisions | Constraints the worker must not reopen |
| Completion criteria | Checkable `CC-NN` statements |

```markdown
---
spec: wanax.contract
version: 1
name: "add-fn"
test_command: "cargo test"
test_timeout_secs: 120
allowed_globs:
  - "src/**"
forbidden_globs:
  - "**/.env"
---

## Intent

Add the missing `add` function so the unit test passes.

## Decisions

- Implement `add` in `src/lib.rs` only

## Boundaries

- Allowed: `src/**`

## Completion Criteria

- CC-01: `cargo test` exits 0
- CC-02: the bound test exists and passes (bound_test: `test_add`)
```

`bound_test:` makes a criterion mechanical: the outer worktree runs
`<test_command> -- --list` (cargo) to prove the named test exists — a filter
matching nothing cannot pass vacuously — then `<test_command> <name>
-- --exact` must exit 0, or the accept gate forces rework. A missing test
command, empty allow-list, or empty criteria is `E_CONTRACT_INVALID`.
Dangerous `test_command` values (`rm`, `sudo`, `curl |`, …) are
`E_TEST_COMMAND_FORBIDDEN`.

You write the contract every time. You do not retune the factory every time.

## Configuration

`wanax init` writes `.wanax/config.toml`. Repo config overlays `~/.wanax/config.toml`. Defaults are enough to start; most people change **model** and **base_url** once.

```toml
[commander]
provider = "openai_compat"   # openai | openai_compat | openai_responses | anthropic | claude_cli | codex_cli
model = "gpt-4.1"
base_url = "https://api.openai.com/v1"
# cli_bin = "claude"         # when provider is claude_cli / codex_cli
# cli_args = []

[inner]
provider = "openai_compat"
model = "gpt-4.1-mini"
base_url = "https://api.openai.com/v1"

[reviewer]
# empty → self-review is mechanical only (tombstone: self_review=degraded)

[worker]
adapter = "octoscode"   # octoscode | fake | cmd | claude | codex | acp
octoscode_bin = "octoscode"
claude_bin = "claude"
codex_bin = "codex"
# acp_bin = "claude-code-acp"  # any agent-client-protocol server; drives
# acp_args = []                # initialize → session/new → session/prompt
                              # over stdio NDJSON-RPC (claude-code-acp,
                              # gemini --acp, codex wrappers, …)
# cmd = "my-agent"       # PATH name or absolute path when adapter = "cmd"
# cmd_args = []          # optional; instruction is WANAX_INSTRUCTION, not argv
# egress = "allowlist"   # off | allowlist | isolated — worker/test children get
                        # HTTP(S)_PROXY → a local CONNECT proxy that tunnels
                        # only whitelisted domains; seatbelt also confines
                        # outbound to localhost:proxy (bypass-proof, DNS too)
# egress_allow = ["api.anthropic.com", "*.github.com"]

[lock]
repo_exclusive = true   # false → path-set lock on allowed_globs; disjoint runs may overlap in time

[budget]
max_usd = "5.00"
max_inner_turns = 40

[test]
default_command = "cargo test"
# pass_env = []            # extra env keys allowed through to test_command (secrets never pass)

[verify]
baseline_test = true    # run test_command on the untouched base before dispatch

[sandbox]
mode = "auto"            # auto | off | seatbelt | bwrap
extra_write_paths = []   # extra writable dirs for workers/tests/commander CLI
```

Keys come only from the environment. They are never written to git or to the tombstone.

| Variable | Used for |
|---|---|
| `WANAX_COMMANDER_API_KEY` | Outer HTTP dispatch and verdict (not used for `claude_cli` / `codex_cli`) |
| `WANAX_INNER_API_KEY` | Semantic self-review when `reviewer.model` ≠ `inner.model` |
| `WANAX_DATA_DIR` | SQLite home (default `~/.wanax`) |

`provider = "openai_compat"` plus `base_url` is the hook for any Chat Completions host; `openai_responses` targets the Responses API (`/v1/responses`, the current surface for o-*/gpt-5 reasoning models). There is no separate `openrouter` provider. Consumer chat subscriptions (including SuperGrok) are a different ledger; Wanax talks to the HTTP API only.

### OpenRouter

Point both loops at `https://openrouter.ai/api/v1` and use catalog slugs from [openrouter.ai/models](https://openrouter.ai/models):

```toml
[commander]
provider = "openai_compat"
model = "anthropic/claude-opus-5"    # dispatch + verdict only
base_url = "https://openrouter.ai/api/v1"

[inner]
provider = "openai_compat"
model = "z-ai/glm-5.3-flash"         # tombstone label; semantic review only if reviewer.model is set and distinct
base_url = "https://openrouter.ai/api/v1"
```

```bash
export WANAX_COMMANDER_API_KEY="$OPENROUTER_API_KEY"
```

`wanax init` writes placeholder `model = "commander"` / `model = "inner"` into the **repo** file. Those init placeholders do not replace a real commander/inner block in `~/.wanax/config.toml`. Set models once globally, then `export WANAX_COMMANDER_API_KEY`.

Without a commander key, a CLI commander, or a fixture, start falls back to a mechanical commander (useful for CI). Live keys are required for a real HTTP outer model.

### Real `cmd` workers

`cmd` does not put the work unit on argv. Point `worker.cmd` at a wrapper that reads `WANAX_INSTRUCTION` and starts a non-interactive coding CLI. Examples in this repo:

```toml
[worker]
adapter = "cmd"
cmd = "/absolute/path/to/wanax/scripts/workers/claude.sh"
# or: cmd = "/absolute/path/to/wanax/scripts/workers/codex.sh"
```

`claude.sh` uses `--dangerously-skip-permissions` so the run does not stop for tool approval. Use it only inside a Wanax inner worktree (no push, tokens already stripped). `codex.sh` uses `--sandbox workspace-write`. The factory still runs `test_command` itself after the process exits.

### Isolation model (defense in depth)

Every child Wanax spawns — worker CLI, `test_command`, commander CLI, verifier plugins — gets a whitelist environment (`PATH HOME USER TMPDIR LANG LC_ALL TERM SHELL` plus contract-declared `test.pass_env`; never `GH_TOKEN`, `SSH_AUTH_SOCK`, or `WANAX_*_API_KEY`), a `GIT_CONFIG_GLOBAL` pointing at a wanax-generated config with an empty credential helper and signing off, and its own process group so timeouts and `wanax cancel` kill the whole tree.

On top of that, `[sandbox] mode = "auto"` confines file access with the OS sandbox — `sandbox-exec` (Seatbelt) on macOS, `bwrap` on Linux: the child may write only its worktree, the repo's real `.git` dir, the run dir, TMPDIR, and standard toolchain caches (`~/.cargo`, `~/.rustup`, `~/.cache`, …; extend via `extra_write_paths`), and may not read `~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.kube`, `~/.config/gh`. With no backend available the child runs unsandboxed and the tombstone records `sandbox = "none"`; `wanax doctor` reports the resolved backend.

Keep binding tests **outside** `allowed_globs`. If the tests live in `src/lib.rs` and that path is allowed, a worker can rewrite them to `assert!(true)` and the outer retest will still go green. Prefer `tests/*.rs` plus `allowed_globs = ["src/**"]`. Changing `Cargo.toml` when it is not allowed is already `E_BOUNDARY`.

`start` and `doctor` warn (`E_CONTRACT_TESTS_WRITABLE`) when `allowed_globs` can match `tests/**`. The run still proceeds. `doctor --strict` exits 4.

### Iteration (`wanax improve`)

`start` runs the contract once. `improve` is the RSI loop: every iteration is a complete independent run — new run id, new worktree, full mechanical gates. When a run finishes non-accepted, its evidence (verdict chain, outer exit codes, boundary violations, error codes) is packed into the next run's dispatch. The contract file is never touched — iteration changes strategy, not spec.

```bash
wanax improve --contract specs/add.contract.md --iterations 3   # default 3, max 8
wanax improve --contract ... --from <run_id>                    # resume an evidence chain from an earlier terminal run (same contract only)
wanax improve --contract ... --adapters cmd,claude              # adapter escalation ladder: iteration i uses adapters[i-1]
wanax improve --contract ... --max-total-usd 8.00               # loop-level spend cap across ALL iterations
wanax report                                                   # fitness over all runs (state, accept rate, per-contract)
wanax report <run_id>                                          # per-run detail incl. parent chain + baseline_green flag
wanax report --json                                            # machine-readable fitness for external loops
wanax report --suggest                                         # rule-based triage: is the bottleneck the spec or the implementation
```

Each iteration's `run_started` tombstone records `iteration` and `parent_run_id`, so the chain is auditable. `improve` exits 0 on the first accepted iteration, `E_REWORK_LIMIT` when the budget of iterations is spent — or earlier when two consecutive iterations produce an identical failure signature (zero progress).

### Persistent learnings

Every finished run appends one distilled line to `.wanax/LEARNINGS.md` (state, verdict chain, outer exit codes, error histogram, sandbox backend). The last 20 lines are injected into each dispatch as "Accumulated learnings" — memory persists across runs and contracts, not just inside an `improve` chain. `.wanax/` is gitignored; learnings are local evidence, never spec.

### Housekeeping (`wanax clean`)

Runs leave worktrees and `wanax/<id>/*` branches behind. `clean` removes them for finished, non-accepted runs:

```bash
wanax clean --dry-run        # preview
wanax clean                  # remove worktrees + branches of finished runs
wanax clean --runs           # also remove run tombstone dirs
wanax clean --include-accepted   # also clean accepted runs (their branch is the output — default keeps it)
wanax clean --include-dead       # also reclaim crash-orphan runs (non-terminal, dead pid)
```

## Commands

| Command | Purpose |
|---|---|
| `wanax init [--force]` | Create `.wanax/` and an example contract |
| `wanax start --contract <path> [--adapter octoscode\|fake\|cmd\|claude\|codex\|acp] [--allow-dirty]` | Freeze the contract, lock the repo, run the factory |
| `wanax resume [run_id]` | Continue an interrupted non-terminal run (takes over a stale LOCK) |
| `wanax status [run_id]` | State, spend, current unit, last event |
| `wanax cost [run_id]` | Local spend board from SQLite / tombstone ticks (no network) |
| `wanax cancel <run_id>` | SIGTERM the worker, keep the tombstone, release the lock |
| `wanax verdict <run_id>` | Print the last outer verdict |
| `wanax report [run_id]` | Fitness view: state, verdicts, baseline/outer exits, errors, cost, wall time |
| `wanax improve --contract <path> [--iterations N]` | RSI loop: rerun with evidence carried across iterations |
| `wanax check --contract <path>` | Mechanical contract preflight: bound_test names vs `cargo test -- --list`, glob coverage, vacuous-green traps, adapter availability. Exit 4 on FAIL |
| `wanax audit <run_id>` | Tombstone integrity: per-event payload_sha256, sequence/id invariants, terminal-state evidence, DB cross-check. Exit non-zero on FAIL |
| `wanax clean [--dry-run] [--runs] [--include-accepted]` | Remove finished-run worktrees and branches |
| `wanax doctor [--strict] [--fix-lock]` | Git, adapter, keys (presence only), stale lock, sandbox, disk |

Global: `--data-dir <path>` (default `~/.wanax`), `--lang en|zh` (or `WANAX_LANG`).

## Outputs

Each run lives under `.wanax/runs/<run_id>/`:

| File | Role |
|---|---|
| `envelope.json` | Source of truth (append-only events) |
| `TOMBSTONE.md` | Rendered audit log |
| `RESULT.md` | Decision, SHA, test excerpt (after accept/reject) |
| `wanax.log` | Local process log (redacted) |

Git artifacts: `wanax/<run_id>/inner` and `wanax/<run_id>/outer`. Inner worktrees cannot `git push` or check out protected refs (`main` / `master` by default).

## Current status

Implemented through **v1 / Phase 6** of [`docs/PRD.md`](docs/PRD.md):

- One work unit per run by default; Commander may emit a peer batch or a WorkUnit DAG
- Goal loop (`plan → edit → test → self_review`, max 8)
- Outer retest on a fresh worktree, glob boundaries, USD + turn budget
- HTTP commander (Anthropic Messages or OpenAI-compatible Chat Completions)
- Cassette fixtures via `WANAX_LLM_FIXTURE_DIR` (CI does not call a paid API)
- Workers: `octoscode`, `fake`, `cmd`, first-class `claude` / `codex` (instruction on stdin; `.sh` wrappers remain examples)
- Goal stops when a red inner test makes no file progress; expensive commander verdict runs only when FR-014 gates are green
- Peer worktrees with overlap rejection and cherry-pick recovery; optional `gh pr create` after accept
- `wanax resume` after a crash / stale lock; `--lang zh`; optional `agent-spec lifecycle` plugin (`[verify] plugins = ["agent-spec"]`)
- Opt-in path-set multi-run (`[lock] repo_exclusive = false`); `wanax cost` local spend board
- CI: Ubuntu 24.04 + macOS, rustc 1.97.1, `cargo build --locked`
- Commander may be HTTP or a local CLI (`claude_cli` / `codex_cli`); agent-spec plugin runs `lint` + `verify` + `lifecycle`
- v1.1 hardening: whitelist child env (`[test] pass_env`), per-run `GIT_CONFIG_GLOBAL`, process-group kills, OS sandbox (`[sandbox] seatbelt/bwrap`, `doctor` reports), real claude/codex usage metering, baseline test (`[verify] baseline_test`), concurrent peers, structured commander output
- v1.2 RSI: `wanax improve` evidence loop, `wanax report` fitness view, `wanax clean` housekeeping; `specs/wanax-self.contract.md` lets Wanax work on itself

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- --deny warnings
```

The Phase 1–2 factory loop is covered by `crates/wanax-cli/tests/e2e_fake_factory.rs`. Implementers should treat [`docs/PRD.md`](docs/PRD.md) as the spec: implement only the current phase; when the spec says `[NEEDS CLARIFICATION]`, do less, do not invent product behavior.

## License

[MIT](LICENSE) © 2026 Henry Zhang
