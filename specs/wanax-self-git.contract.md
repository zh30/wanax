---
spec: wanax.contract
version: 1
name: "self-git-coverage"
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

`crates/wanax-git/src/lib.rs` wraps every git op the factory
depends on — but has exactly one test (`dirty_ignores_wanax`). The
`init_repo` helper already exists inside `#[cfg(test)] mod tests`.
Expand that module to cover the isolation surface:

- `create_branch` + `ref_exists` (true for new branch, false for
  unknown)
- `worktree_add_branch` + `worktree_remove_force` — worktree dir
  appears then disappears
- `commit` + `status_paths` + `add_files` — write file, add it,
  commit, `status_paths` is clean and HEAD moved
- `diff_name_only` + `diff_stat` + `diff_patch` between HEAD and a
  commit that adds a file — each reports the file
- `is_ancestor` — first commit is ancestor of second, not vice
  versa
- `reset_branch` — point branch at earlier sha, `head_sha` via the
  branch ref changes

Each test: init_repo() → arrange → act → assert. Reuse the existing
helpers; no new deps.

## Decisions

- Only `crates/wanax-git/src/lib.rs`'s test module should change

## Boundaries

- Allowed: crates/**

## Completion Criteria

- CC-01: `cargo test --workspace` exits 0
- CC-02: the worktree lifecycle test exists and passes (bound_test: `worktree_add_remove_cycle`)
- CC-03: no file outside `crates/**` is modified
