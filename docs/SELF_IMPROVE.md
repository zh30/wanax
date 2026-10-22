# Self-improvement recipe — Wanax on Wanax

Wanax can improve itself: the factory treats its own repo as the job
site. Verified end-to-end (opus commander + claude CLI worker →
rework → rework → accepted → inner branch carrying real test code,
merged into main). This document is the repeatable recipe.

## What works

```
specs/wanax-self.contract.md          — generic template (intent left
                                        open; narrow before dispatch)
specs/wanax-self-isforbidden.contract.md
                                      — concrete run: add unit tests
                                        for envenv::is_forbidden
```

The self-contract's boundaries keep the worker inside `crates/**`
while specs/CI/docs stay human territory; `test_command = "cargo test
--workspace"` makes the full suite the outer gate — this is what
surfaced the nested-Seatbelt bug (WANAX_SANDBOXED, #40/#41).

## Recipe

```bash
cd wanax                       # the wanax repo itself
WANAX_COMMANDER_API_KEY=$OPENROUTER_API_KEY \
wanax start --contract specs/wanax-self-<topic>.contract.md \
            --adapter claude   # or cmd / codex — any agent you trust
# then:
wanax watch <run_id>           # or wait for notify_cmd
wanax diff  <run_id>           # what it actually changed
wanax audit <run_id>           # envelope integrity
git merge --no-ff wanax/<run_id>/inner   # human decision, as always
```

One focused variant contract per run — copy
`wanax-self-isforbidden.contract.md` and narrow the Intent/Decisions
sections to a single bounded goal (a missing test, a dead-code
removal, a small refactor). Sweeping refactors are out of scope for a
single work unit by design.

## Cadence

Wanax is local and foreground; the loop is human-paced. A simple
cron/launchd entry that runs `wanax improve --contract <self> --adapter
<agent> --iterations 2 --max-total-usd 3` weekly is enough — the
accepted inner branch + `WANAX_CREATE_PR=1` opens the PR; you review
and merge. Never auto-merge.

## Hard rules (unchanged for self-runs)

- Contracts are frozen per run; strategy iterates, specs don't.
- Mechanical gates always hold — a self-run that weakens
  sandbox/env/boundary enforcement to go green is invalid by
  definition (the contract's Decisions section forbids it, and the
  outer test would still catch regressions).
- Final merge is always human. `[github] create_pr` opens the PR;
  nothing ever merges `main` for you.
