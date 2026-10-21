//! `wanax clean` — remove per-run leftovers for finished runs.
//!
//! Every run leaves a worktree (`<id>-inner`, `<id>-outer`, `<id>-peer-N`)
//! and branches (`wanax/<id>/*`) behind. Accepted runs are kept by default —
//! their branch is the deliverable — everything else terminal is garbage.
//! Active runs and their locks are never touched.

use std::path::{Path, PathBuf};
use wanax_core::error::{ErrorCode, WanaxError};
use wanax_core::lock::{pid_alive, release_run_lock};
use wanax_core::types::RunState;
use wanax_core::Store;

pub struct CleanOpts {
    pub data_dir: PathBuf,
    pub include_accepted: bool,
    /// Also clean runs stuck non-terminal whose owning process is dead
    /// (crash orphans). Without this they are skipped conservatively.
    pub include_dead: bool,
    /// Also remove the run's tombstone dir and lock holders. Tombstones are
    /// the audit trail, so this is opt-in.
    pub runs: bool,
    pub dry_run: bool,
}

pub async fn run(opts: CleanOpts) -> Result<(), WanaxError> {
    let store = Store::open(&opts.data_dir.join("wanax.db")).await?;
    let mut cleaned = 0u32;
    let mut kept = 0u32;
    for r in store.list_runs().await? {
        if !r.state.is_terminal() {
            // Non-terminal + dead owner pid = crash orphan, cleanable on
            // request. A live process always wins — never touch it.
            let dead = r
                .start_pid
                .map(|p| !pid_alive(p as u32))
                .unwrap_or(false);
            if !(opts.include_dead && dead) {
                println!("skip {} (active state={})", r.id, r.state.as_str());
                continue;
            }
            println!("clean {} (dead run, state={})", r.id, r.state.as_str());
        }
        if r.state == RunState::Accepted && !opts.include_accepted {
            println!("keep {} (accepted)", r.id);
            kept += 1;
            continue;
        }
        let repo = PathBuf::from(&r.repo_root);
        if !repo.join(".git").exists() {
            println!("skip {} (repo gone: {})", r.id, r.repo_root);
            continue;
        }
        clean_worktrees(&repo, &r.id, opts.dry_run);
        clean_branches(&repo, &r.id, opts.dry_run);
        let _ = release_run_lock(&repo, &r.id);
        if opts.runs {
            let dir = repo.join(".wanax").join("runs").join(&r.id);
            remove_dir(&dir, opts.dry_run);
        }
        cleaned += 1;
    }
    println!(
        "clean: {} run(s) cleaned, {} kept (accepted){}",
        cleaned,
        kept,
        if opts.dry_run { " [dry-run]" } else { "" }
    );
    Ok(())
}

fn remove_dir(dir: &Path, dry_run: bool) {
    if !dir.is_dir() {
        return;
    }
    if dry_run {
        println!("would remove {}", dir.display());
    } else {
        let _ = std::fs::remove_dir_all(dir);
        println!("removed {}", dir.display());
    }
}

/// Worktree dirs are `<repo>/.wanax/worktrees/<run_id>-{inner,outer,peer-N}`.
fn clean_worktrees(repo: &Path, run_id: &str, dry_run: bool) {
    let wt_root = repo.join(".wanax").join("worktrees");
    let prefix = format!("{run_id}-");
    let Ok(entries) = std::fs::read_dir(&wt_root) else {
        return;
    };
    for e in entries.filter_map(|e| e.ok()) {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&prefix) || !e.path().is_dir() {
            continue;
        }
        let path = e.path();
        if dry_run {
            println!("would remove worktree {}", path.display());
            continue;
        }
        let ok = std::process::Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&path)
            .current_dir(repo)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            println!("removed worktree {}", path.display());
        } else {
            // Half-registered worktree: drop the dir and let git prune.
            let _ = std::fs::remove_dir_all(&path);
            let _ = std::process::Command::new("git")
                .args(["worktree", "prune"])
                .current_dir(repo)
                .status();
            println!("removed worktree {} (forced)", path.display());
        }
    }
}

/// Only refs under `refs/heads/wanax/<run_id>/` are ever candidates.
fn clean_branches(repo: &Path, run_id: &str, dry_run: bool) {
    let prefix = format!("refs/heads/wanax/{run_id}/");
    let out = std::process::Command::new("git")
        .args([
            "for-each-ref",
            &prefix,
            "--format=%(refname:strip=2)",
        ])
        .current_dir(repo)
        .output();
    let Ok(out) = out else { return };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let branch = line.trim();
        if branch.is_empty() {
            continue;
        }
        if dry_run {
            println!("would delete branch {branch}");
            continue;
        }
        let ok = std::process::Command::new("git")
            .args(["branch", "-D", branch])
            .current_dir(repo)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            println!("deleted branch {branch}");
        } else {
            eprintln!(
                "WARN {} failed to delete branch {branch}",
                ErrorCode::Db.as_str()
            );
        }
    }
}
