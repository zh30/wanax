---
spec: wanax.contract
version: 1
name: "self-money-coverage"
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

Expand unit test coverage in `crates/wanax-core/src/money.rs`'s
`#[cfg(test)] mod tests` for the parse/format edges currently
unexercised:

- `parse_usd_decimal` rejects: empty string, non-digit whole part,
  non-digit fraction, more than 6 fraction digits
- `parse_usd_decimal` accepts a negative amount ("-1.5" → -1_500_000)
- `format_usd` renders negative micros ("-2500000" → "-2.500000")
- `format_usd_4` truncates to 4 digits (5_000_000 → "5.0000",
  65_000 → "0.0650")
- `estimate_cost_micros` rounds down with mixed in/out usage

Only add `#[test]` functions inside the existing tests module — no
behaviour changes, no new files, no new dependencies.

## Decisions

- Only `crates/wanax-core/src/money.rs` should change
- Match existing test style (plain #[test] fns)

## Boundaries

- Allowed: crates/**

## Completion Criteria

- CC-01: `cargo test --workspace` exits 0
- CC-02: the negative-parse test exists and passes (bound_test: `parse_rejects_negative_and_malformed`)
- CC-03: no file outside `crates/**` is modified
