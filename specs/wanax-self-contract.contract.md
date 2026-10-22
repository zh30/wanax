---
spec: wanax.contract
version: 1
name: "self-contract-parser-coverage"
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

`crates/wanax-core/src/contract.rs` is the front door of the whole
system — if `parse_contract_bytes` mis-parses, every downstream gate
works on a wrong contract. It has only two tests today. Expand
`#[cfg(test)] mod tests` to cover the real grammar surface:

- missing front matter (`spec: wanax.contract` header absent) → Err
- `version` mismatch or unparseable → Err
- `allowed_globs`/`forbidden_globs` YAML list parsing incl. quoted
  entries and `**` patterns
- `## Boundaries` bullet lists ("允许："/"Allowed:" aliases)
  populate the same fields
- `## Completion Criteria` lines: `- CC-01: ...` ids captured in
  order; `bound_test:` backtick annotation extracted; `must_have:`
  comma-separated paths extracted
- intent/decisions sections round-trip into `intent`/`decisions`
- `content_sha256` is deterministic for identical bytes, differs
  when one byte differs

Each test constructs the minimal contract string it needs (reuse
the `SAMPLE` constant style already in the file) — no new deps.

## Decisions

- Only `crates/wanax-core/src/contract.rs`'s test module changes

## Boundaries

- Allowed: crates/**

## Completion Criteria

- CC-01: `cargo test --workspace` exits 0
- CC-02: a parse edge-case test exists and passes (bound_test: `parse_rejects_missing_front_matter`)
- CC-03: no file outside `crates/**` is modified
