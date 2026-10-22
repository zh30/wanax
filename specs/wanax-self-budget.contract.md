---
spec: wanax.contract
version: 1
name: "self-budget-coverage"
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

`crates/wanax-core/src/budget.rs` is the spend gate — it decides
when a run must stop burning money. ~90 lines, 3 tests, and every
decision is arithmetic with real failure modes. The existing tests
module has a `run(max_usd, max_turns)` fixture. Expand it to cover:

- `would_exhaust_usd` — false below cap, true at exactly cap+1,
  true when additional would cross it
- `is_usd_exhausted` / `is_turns_exhausted` /
  `is_budget_exhausted` — each boundary (spent==max)
- `charge_usd_micros` accumulates; `add_turns` accumulates;
  BudgetTick fields reflect the post-charge state
- `charge_estimated` vs `charge_units` semantics (read the impl
  first — one is a projected estimate, the other books real usage)
- `budget_error` returns a WanaxError whose code/message mentions
  the exhausted dimension

## Decisions

- Only `crates/wanax-core/src/budget.rs`'s test module changes;
  reuse the `run()` fixture

## Boundaries

- Allowed: crates/**

## Completion Criteria

- CC-01: `cargo test --workspace` exits 0
- CC-02: the budget-boundary test exists and passes (bound_test: `budget_exhaustion_boundaries`)
- CC-03: no file outside `crates/**` is modified
