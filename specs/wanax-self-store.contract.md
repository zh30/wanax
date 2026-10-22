---
spec: wanax.contract
version: 1
name: "self-store-coverage"
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

`crates/wanax-core/src/store.rs` (the SQLite run/work-unit/verdict
cache, ~571 lines) has ZERO unit tests — its coverage is only
incidental through the e2e suite. Add a `#[cfg(test)] mod tests` at
the bottom of the file covering the round-trip surface with a
`tempfile::tempdir()` sqlite db:

- `open` + `insert_contract` + `get_contract` round trip
- `insert_run` + `get_run` + `list_runs` (state field round trip)
- `set_state` mutates and is visible through `get_run`
- `insert_work_unit` + `get_work_unit` + `work_units_for_run`
- `insert_verdict` + `latest_verdict` returns the newest
- missing-row lookups (`get_run` on unknown id) return an error, not
  a panic

Tests are async — `#[tokio::test]` (tokio is already a workspace
dependency of wanax-core; check Cargo.toml before assuming). Read
the struct shapes (Contract, FactoryRun, WorkUnit, Verdict, Receipt)
in `crates/wanax-core/src/types.rs` first — construct minimal
instances; every field must be set.

## Decisions

- Only `crates/wanax-core/src/store.rs` should change — a new test
  module at the bottom of the file
- If a `tempfile` dev-dependency is missing, check whether
  wanax-core already has one (look at Cargo.toml); if absent, use
  `std::env::temp_dir()` + a unique subdir instead of adding deps

## Boundaries

- Allowed: crates/**

## Completion Criteria

- CC-01: `cargo test --workspace` exits 0
- CC-02: the store round-trip test exists and passes (bound_test: `run_insert_get_roundtrip`)
- CC-03: no file outside `crates/**` is modified
