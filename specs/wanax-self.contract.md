---
spec: wanax.contract
version: 1
name: "wanax-self-improve"
test_command: "cargo test --workspace"
test_timeout_secs: 900
allowed_globs:
  - "crates/**"
forbidden_globs:
  - "specs/**"
  - ".github/**"
  - "docs/**"
---

## Intent

Improve the Wanax codebase itself. Wanax dogfoods its own factory: the inner
worker edits `crates/**` while the outer loop mechanically re-verifies with
the workspace test suite.

Pick a concrete, bounded improvement per run — the intent section of a
self-contract should be narrowed to one goal before dispatch (e.g. "reduce
duplicate error mapping in engine.rs", "add a missing unit test for
budget charge edge cases"). This template stays generic; write a focused
variant per actual run.

## Decisions

- Work happens only under `crates/**` — specs, CI config, and docs are
  human/outer-loop territory
- Do not weaken any mechanical gate (accept rules, boundary checks, env
  sanitization, sandbox, budget) to make a test go green
- Keep `cargo clippy --workspace --all-targets -- --deny warnings` clean;
  tests that rely on it failing are out of scope for a single work unit
- Small, reviewable diffs preferred over sweeping refactors

## Boundaries

- Allowed: crates/**
- Forbidden: specs/**, .github/**, docs/**

## Completion Criteria

- CC-01: `cargo test --workspace` exits 0 on the worker's commit
- CC-02: the money-format unit test exists and passes (bound_test: `money::tests::default_budget_is_five_dollars`)
- CC-02: no file outside `crates/**` is modified
- CC-03: the diff is non-empty and inside the allowed globs
