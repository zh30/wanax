# Wanax contributor notes

## Verification

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- --deny warnings
```

## Workflow

- Each finished feature block gets its own PR (stacked when it depends on
  an earlier unmerged block). Do not batch unrelated features into one PR.
- Tombstone/envelope events are the audit boundary — new gate-affecting
  behavior needs a matching `EventKind` and a `wanax report` surface.
- Contracts are frozen for a run's lifetime; strategy iterates, specs don't.
