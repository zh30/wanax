# Wanax contributor notes

## Verification

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- --deny warnings
cargo fmt --all -- --check   # CI-gated; run fmt before every PR
```

## Workflow

- Each finished feature block gets its own PR (stacked when it depends on
  an earlier unmerged block). Do not batch unrelated features into one PR.
- Tombstone/envelope events are the audit boundary — new gate-affecting
  behavior needs a matching `EventKind` and a `wanax report` surface.
- Contracts are frozen for a run's lifetime; strategy iterates, specs don't.
- Model *inputs* are audit evidence too: any `LlmUsage` carrying a prompt
  must set `prompt_in` so `charge_and_tick` persists it under
  `.wanax/runs/<id>/prompts/` with `prompt_sha256` in the budget tick.

## Stacked PRs

When PR B is based on PR A's branch: merge A into main **without**
`--delete-branch`, retarget B to main (`gh pr edit B --base main`), then
merge B. Deleting the base first makes GitHub auto-close B unmerged
(happened twice). Merge bottom-up, retarget before each merge, delete all
branches at the end.

## "Declared but never implemented" pattern

Spec fields/flags that parse but no one executes are the recurring bug
class here (`bound_test`, `must_have_files`, `[run] notify_cmd` all were).
When adding a surface, grep the struct field for its *consumer* — if only
the parser reads it, the feature isn't done.

## Dogfood (real-model smoke)

```bash
cd /tmp/dogfood-repo            # scratch cargo fixture, committed clean
WANAX_DATA_DIR=/tmp/dogfood-data \
WANAX_COMMANDER_API_KEY=$OPENROUTER_API_KEY \
wanax start --contract specs/mul.contract.md --adapter cmd
# then: wanax report / audit / replay / export <run_id>
```

Commander needs the OpenRouter `openai_compat` config in the repo's
`.wanax/config.toml`; the `cmd` worker reads its own key file (or
`[worker] pass_env`) — wanax never injects `WANAX_*_API_KEY`.
