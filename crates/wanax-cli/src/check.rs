//! `wanax check` — mechanical contract preflight, before a run burns budget.
//!
//! The contract is the human's one lever; its quality is their call — but
//! the tools that keep biting people (bound tests named wrong → never
//! accepts, `tests/` inside allowed_globs → vacuous green, globs matching
//! zero files, a test_command binary that doesn't exist) are all
//! mechanically detectable. `check` runs them and exits non-zero on any
//! FAIL so it can gate a start command or CI.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use wanax_core::config::load_merged_config;
use wanax_core::contract::parse_contract_file;
use wanax_core::envenv;
use wanax_core::error::{ErrorCode, WanaxError};
use wanax_core::types::WorkerAdapterKind;
use wanax_verify::{allowed_globs_cover_binding_tests, compile_globs, run_test_command};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Pass,
    Warn,
    Fail,
}

impl Level {
    fn marker(self) -> &'static str {
        match self {
            Level::Pass => " ok ",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        }
    }
}

struct Item {
    level: Level,
    label: &'static str,
    message: String,
}

pub struct CheckOpts {
    pub contract: PathBuf,
    pub adapter: Option<WorkerAdapterKind>,
    pub data_dir: PathBuf,
}

fn bin_on_path(name: &str) -> bool {
    if name.starts_with('/') {
        return Path::new(name).is_file();
    }
    std::env::var("PATH").ok().is_some_and(|p| {
        p.split(':').any(|d| Path::new(d).join(name).is_file())
    })
}

/// Tracked files matching any allowed glob (empty → the contract likely
/// names the wrong paths).
fn matching_files(repo: &Path, allowed: &[String]) -> usize {
    let Ok(set) = compile_globs(allowed) else {
        return 0;
    };
    let out = std::process::Command::new("git")
        .args(["-C", &repo.display().to_string(), "ls-files"])
        .output();
    let Ok(out) = out else { return 0 };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|f| set.is_match(f))
        .count()
}

/// The names a bound_test must equal (or end-match) — via the contract's own
/// test_command -- --list, same semantics as the outer gate. None when the
/// test runner can't enumerate (non-cargo or a failed listing).
fn listed_test_names(
    repo: &Path,
    test_command: &str,
    timeout_secs: u32,
) -> Result<Option<BTreeSet<String>>, WanaxError> {
    if !test_command.starts_with("cargo test") {
        return Ok(None);
    }
    let run = run_test_command(
        repo,
        &format!("{test_command} -- --list"),
        timeout_secs.min(120),
        &envenv::base_child_env(),
        None,
    )?;
    if run.exit_code != 0 {
        return Ok(None);
    }
    let names = run
        .excerpt
        .lines()
        .filter_map(|l| l.trim().strip_suffix(": test").map(|s| s.trim().to_string()))
        .collect();
    Ok(Some(names))
}

pub async fn run(opts: CheckOpts) -> Result<(), WanaxError> {
    let repo = std::env::current_dir().map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    let cfg = load_merged_config(&repo, &opts.data_dir)?;
    let mut items: Vec<Item> = Vec::new();
    let mut push = |level: Level, label: &'static str, message: String| {
        items.push(Item { level, label, message });
    };

    // 1. Parse.
    let rel = opts
        .contract
        .strip_prefix(&repo)
        .unwrap_or(&opts.contract)
        .display()
        .to_string();
    let contract = match parse_contract_file(&opts.contract, &rel) {
        Ok(c) => {
            push(
                Level::Pass,
                "parse",
                format!("contract parses ({} criteria)", c.completion_criteria.len()),
            );
            c
        }
        Err(e) => {
            push(Level::Fail, "parse", format!("{e}"));
            report(&opts.contract, &items);
            return Err(e);
        }
    };

    // 2. Globs valid + cover real files.
    match compile_globs(&contract.allowed_globs) {
        Ok(_) => push(Level::Pass, "globs", "allowed_globs compile".into()),
        Err(e) => push(Level::Fail, "globs", format!("allowed_globs: {e}")),
    }
    if contract.allowed_globs.is_empty() {
        push(Level::Fail, "allow", "allowed_globs is empty — nothing a worker may write".into());
    } else {
        let n = matching_files(&repo, &contract.allowed_globs);
        if n == 0 {
            push(Level::Warn, "allow", format!(
                "allowed_globs {} match 0 tracked files — wrong paths?",
                contract.allowed_globs.join(",")
            ));
        } else {
            push(Level::Pass, "allow", format!("allowed_globs cover {n} tracked files"));
        }
    }
    if allowed_globs_cover_binding_tests(&contract.allowed_globs) {
        push(Level::Warn, "allow", "allowed_globs cover tests/ — a worker could rewrite the binding tests (vacuous-green trap)".into());
    }

    // 3. Forbidden globs.
    if contract.forbidden_globs.is_empty() {
        push(Level::Warn, "forbid", "no forbidden_globs — secrets (**.env, credentials) unprotected".into());
    } else if compile_globs(&contract.forbidden_globs).is_err() {
        push(Level::Fail, "forbid", "forbidden_globs do not compile".into());
    } else {
        push(Level::Pass, "forbid", format!("{} forbidden pattern(s)", contract.forbidden_globs.len()));
    }

    // 4. test_command binary exists.
    let first = contract.test_command.split_whitespace().next().unwrap_or("");
    if first.is_empty() {
        push(Level::Fail, "test", "test_command is empty".into());
    } else if bin_on_path(first) {
        push(Level::Pass, "test", format!("test binary `{first}` on PATH"));
    } else {
        push(Level::Fail, "test", format!("test binary `{first}` not found on PATH"));
    }
    if contract.test_timeout_secs < 30 {
        push(Level::Warn, "test", format!(
            "test_timeout_secs={} is very short — cargo builds can exceed it",
            contract.test_timeout_secs
        ));
    }

    // 5. bound_test criteria: every named test must exist. The same
    // zero-match trap the outer gate defends against is checkable here.
    let bound: Vec<(&str, &str)> = contract
        .completion_criteria
        .iter()
        .filter_map(|c| c.bound_test.as_deref().map(|t| (c.id.as_str(), t)))
        .collect();
    if bound.is_empty() {
        push(Level::Warn, "bound", "no bound_test criteria — a green test suite may be vacuous".into());
    } else {
        match listed_test_names(&repo, &contract.test_command, contract.test_timeout_secs)? {
            None => push(Level::Warn, "bound", format!(
                "could not enumerate tests via `{}` — bound_test existence not verifiable here (outer gate will re-check)",
                contract.test_command
            )),
            Some(names) => {
                let mut missing = Vec::new();
                for (cid, name) in &bound {
                    let found = names.iter().any(|n| {
                        n.as_str() == *name || n.ends_with(&format!("::{name}"))
                    });
                    if found {
                        push(Level::Pass, "bound", format!("{cid}: `{name}` is listed"));
                    } else {
                        missing.push((*cid, *name));
                    }
                }
                for (cid, name) in &missing {
                    push(Level::Fail, "bound", format!(
                        "{cid}: bound_test `{name}` not in `cargo test -- --list` — contract can never accept"
                    ));
                }
            }
        }
    }

    // 6. Intent quality — the parts no machine can enforce, nudged anyway.
    if contract.intent.trim().is_empty() {
        push(Level::Warn, "intent", "## Intent is empty — the worker has no spec beyond globs".into());
    } else {
        push(Level::Pass, "intent", format!("intent {} chars", contract.intent.trim().len()));
    }
    if contract.decisions.is_empty() {
        push(Level::Warn, "intent", "no ## Decisions — ambiguous intent invites rework loops".into());
    }

    // 7. Adapter + config-level issues.
    let kind = opts
        .adapter
        .unwrap_or_else(|| WorkerAdapterKind::parse(&cfg.file.worker.adapter).unwrap_or(WorkerAdapterKind::Fake));
    match crate::engine::preflight_adapter(kind, &cfg) {
        Ok(()) => push(Level::Pass, "adapter", format!("adapter {} available", kind.as_str())),
        Err(e) => push(Level::Fail, "adapter", format!("{e}")),
    }
    let egress = wanax_core::egress::EgressMode::parse(&cfg.file.worker.egress);
    if egress == wanax_core::egress::EgressMode::Allowlist
        && cfg.file.worker.egress_allow.is_empty()
        && cfg.file.commander.base_url.is_none()
        && cfg.file.inner.base_url.is_none()
    {
        push(Level::Warn, "egress", "egress=allowlist with no hosts — every outbound call will be denied".into());
    }

    report(&opts.contract, &items);
    let fails = items.iter().filter(|i| i.level == Level::Fail).count();
    let warns = items.iter().filter(|i| i.level == Level::Warn).count();
    if fails > 0 {
        return Err(WanaxError::new(
            ErrorCode::ContractInvalid,
            format!("contract check failed: {fails} error(s), {warns} warning(s)"),
        ));
    }
    println!("result: PASS ({warns} warning(s))");
    Ok(())
}

fn report(contract: &Path, items: &[Item]) {
    println!("check {}", contract.display());
    for i in items {
        println!("  {} {:<8} {}", i.level.marker(), i.label, i.message);
    }
}
