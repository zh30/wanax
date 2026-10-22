---
spec: wanax.contract
version: 1
name: "example"
test_command: "cargo test"
test_timeout_secs: 300
allowed_globs:
  - "src/**"
forbidden_globs:
  - "**/.env"
  - "**/.wanax/credentials*"
---

## Intent

Describe the change you want the factory to make.

## Decisions

- Decision 1: replace this with a real decision
- Keep binding tests in `tests/` so they stay outside `allowed_globs`

## Boundaries

- Allowed: `src/**` (implementation only)
- Not allowed: `tests/**` — a worker that rewrites tests will be rejected
- Forbidden: secrets and credentials

## Completion Criteria

- CC-01: `cargo test` exits 0
