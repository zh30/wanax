---
spec: wanax.contract
version: 1
name: "self-is-forbidden-tests"
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

Add focused unit tests for `wanax_core::envenv::is_forbidden`
(introduced by the worker pass_env feature): it must return true for
each name in FORBIDDEN_ENV (GH_TOKEN, SSH_AUTH_SOCK,
WANAX_COMMANDER_API_KEY, WANAX_INNER_API_KEY, …) and false for benign
names like PATH, HOME, and a hypothetical agent CLI key. Put the
tests in `crates/wanax-core/src/envenv.rs`'s existing `#[cfg(test)]
mod tests`.

## Decisions

- Only `crates/wanax-core/src/envenv.rs` should change — this is a
  narrow test-addition unit
- Match the existing test style in that file (#[test] fns, no extra
  deps)

## Boundaries

- Allowed: crates/**

## Completion Criteria

- CC-01: `cargo test --workspace` exits 0
- CC-02: `is_forbidden` coverage exists in the envenv test module (bound_test: `is_forbidden_blocks_known_names`)
- CC-03: no file outside `crates/**` is modified
