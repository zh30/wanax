---
spec: wanax.contract
version: 1
name: "self-types-coverage"
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

`crates/wanax-core/src/types.rs` (~365 lines) is the vocabulary the
whole factory speaks — RunState/WorkUnitState/VerdictDecision/
WorkerAdapterKind/AssigneeRole — but has exactly one test. Its
`as_str`/`parse`/`is_terminal`/`is_supported` contract is what the
SQLite cache, tombstone envelope, and every match arm depend on.
Expand `#[cfg(test)] mod tests` to cover:

- `RunState::parse`/`as_str` round trip for EVERY variant
  (draft→accepted); unknown strings → None
- `is_terminal` for every variant — rework is not terminal,
  accepted/rejected/escalate/failed/cancelled are
- `WorkUnitState::parse` round trip; `parse` of a bogus string →
  None
- `WorkerAdapterKind::parse` round trip + `is_supported` reflects
  the enum's own list (fake/cmd/claude/codex/acp supported; a
  fabricated kind can't round-trip)
- `VerdictDecision::parse`/`as_str` round trip
- `inner_branch_name`/`outer_branch_name` embed the run id

One #[test] per surface; exhaustive-match loops where the enum is
small.

## Decisions

- Only `crates/wanax-core/src/types.rs`'s test module should change

## Boundaries

- Allowed: crates/**

## Completion Criteria

- CC-01: `cargo test --workspace` exits 0
- CC-02: the state-machine test exists and passes (bound_test: `run_state_parse_roundtrip`)
- CC-03: no file outside `crates/**` is modified
