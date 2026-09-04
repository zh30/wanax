use std::fs;
use wanax_core::config::default_config_toml;
use wanax_core::error::{ErrorCode, WanaxError};
use wanax_git::is_git_repo;

const GITIGNORE: &str = "worktrees/\nLOCK\nLOCKSET\nlocks/\n*.db\ncredentials*\n";

const EXAMPLE_CONTRACT: &str = r#"---
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
"#;

pub fn run(force: bool) -> Result<(), WanaxError> {
    let cwd = std::env::current_dir().map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    if !is_git_repo(&cwd) {
        return Err(WanaxError::from_code(ErrorCode::NotGit));
    }
    let config = cwd.join(".wanax").join("config.toml");
    if config.is_file() && !force {
        return Err(WanaxError::from_code(ErrorCode::AlreadyInit));
    }
    fs::create_dir_all(cwd.join(".wanax"))
        .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    fs::create_dir_all(cwd.join("specs")).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    fs::write(&config, default_config_toml())
        .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    fs::write(cwd.join(".wanax").join(".gitignore"), GITIGNORE)
        .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    fs::write(
        cwd.join("specs").join("example.contract.md"),
        EXAMPLE_CONTRACT,
    )
    .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    println!("{}", crate::i18n::t("initialized"));
    println!("{}", crate::i18n::t("init_next"));
    Ok(())
}
