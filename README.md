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
- [Rust](https://rustup.rs/) 1.85+
- Git
- A commander API key (OpenAI-compatible or Anthropic)
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
export WANAX_COMMANDER_API_KEY='…'   # required for a live commander
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
```

A missing test command, empty allow-list, or empty criteria is `E_CONTRACT_INVALID`. Dangerous `test_command` values (`rm`, `sudo`, `curl |`, …) are `E_TEST_COMMAND_FORBIDDEN`.

You write the contract every time. You do not retune the factory every time.

## Configuration

`wanax init` writes `.wanax/config.toml`. Repo config overlays `~/.wanax/config.toml`. Defaults are enough to start; most people change **model** and **base_url** once.

```toml
[commander]
provider = "openai_compat"   # openai | openai_compat | anthropic
model = "gpt-4.1"
base_url = "https://api.openai.com/v1"

[inner]
provider = "openai_compat"
model = "gpt-4.1-mini"
base_url = "https://api.openai.com/v1"

[reviewer]
# empty → self-review is mechanical only (tombstone: self_review=degraded)

[worker]
adapter = "octoscode"   # octoscode | fake | cmd
octoscode_bin = "octoscode"
# cmd = "my-agent"       # PATH name or absolute path when adapter = "cmd"
# cmd_args = []          # optional; instruction is WANAX_INSTRUCTION, not argv

[budget]
max_usd = "5.00"
max_inner_turns = 40
```

Keys come only from the environment. They are never written to git or to the tombstone.

| Variable | Used for |
|---|---|
| `WANAX_COMMANDER_API_KEY` | Outer dispatch and verdict |
| `WANAX_INNER_API_KEY` | Semantic self-review when `reviewer.model` ≠ `inner.model` |
| `WANAX_DATA_DIR` | SQLite home (default `~/.wanax`) |

`provider = "openai_compat"` plus `base_url` is the hook for any Chat Completions host. There is no separate `openrouter` provider. Consumer chat subscriptions (including SuperGrok) are a different ledger; Wanax talks to the HTTP API only.

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

Without a commander key or a fixture, start falls back to a mechanical commander (useful for CI). Live keys are required for a real outer model.

### Real `cmd` workers

`cmd` does not put the work unit on argv. Point `worker.cmd` at a wrapper that reads `WANAX_INSTRUCTION` and starts a non-interactive coding CLI. Examples in this repo:

```toml
[worker]
adapter = "cmd"
cmd = "/absolute/path/to/wanax/scripts/workers/claude.sh"
# or: cmd = "/absolute/path/to/wanax/scripts/workers/codex.sh"
```

`claude.sh` uses `--dangerously-skip-permissions` so the run does not stop for tool approval. Use it only inside a Wanax inner worktree (no push, tokens already stripped). `codex.sh` uses `--sandbox workspace-write`. The factory still runs `test_command` itself after the process exits.

Keep binding tests **outside** `allowed_globs`. If the tests live in `src/lib.rs` and that path is allowed, a worker can rewrite them to `assert!(true)` and the outer retest will still go green. Prefer `tests/*.rs` plus `allowed_globs = ["src/**"]`. Changing `Cargo.toml` when it is not allowed is already `E_BOUNDARY`.

`start` and `doctor` warn (`E_CONTRACT_TESTS_WRITABLE`) when `allowed_globs` can match `tests/**`. The run still proceeds. `doctor --strict` exits 4.

## Commands

| Command | Purpose |
|---|---|
| `wanax init [--force]` | Create `.wanax/` and an example contract |
| `wanax start --contract <path> [--adapter octoscode\|fake\|cmd] [--allow-dirty]` | Freeze the contract, lock the repo, run the factory |
| `wanax status [run_id]` | State, spend, current unit, last event |
| `wanax cancel <run_id>` | SIGTERM the worker, keep the tombstone, release the lock |
| `wanax verdict <run_id>` | Print the last outer verdict |
| `wanax doctor [--strict] [--fix-lock]` | Git, adapter, keys (presence only), stale lock, disk |

Global: `--data-dir <path>` (default `~/.wanax`).

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

Implemented through **Phase 2** of [`docs/PRD.md`](docs/PRD.md):

- One work unit per run, Goal loop (`plan → edit → test → self_review`, max 8)
- Outer retest on a fresh worktree, glob boundaries, USD + turn budget
- HTTP commander (Anthropic Messages or OpenAI-compatible Chat Completions)
- Cassette fixtures via `WANAX_LLM_FIXTURE_DIR` (CI does not call a paid API)
- Workers: `octoscode`, `fake`, `cmd` (generic subprocess; instruction via `WANAX_INSTRUCTION`; example wrappers in `scripts/workers/`)
- Goal stops when a red inner test makes no file progress; expensive commander verdict runs only when FR-014 gates are green

Not in v1 yet: peer worktrees, GitHub PRs, crash resume, multi-unit DAGs, Claude/Codex adapters, `--lang zh`.

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- --deny warnings
```

The Phase 1–2 factory loop is covered by `crates/wanax-cli/tests/e2e_fake_factory.rs`. Implementers should treat [`docs/PRD.md`](docs/PRD.md) as the spec: implement only the current phase; when the spec says `[NEEDS CLARIFICATION]`, do less, do not invent product behavior.

## License

[MIT](LICENSE) © 2026 Henry Zhang
