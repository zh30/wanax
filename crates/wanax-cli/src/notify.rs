//! `[run] notify_cmd` — fire-and-forget terminal notification.
//!
//! A run reaching a terminal state is exactly when the human needs to
//! come back. The command runs once, in the repo, with `WANAX_*` env,
//! inheriting the sanitized child env. It is spawned detached and any
//! failure is swallowed — a notification must never fail a run.

use std::path::Path;
use std::process::{Command, Stdio};
use wanax_core::config::{global_data_dir, load_merged_config};
use wanax_core::envenv::base_child_env;
use wanax_core::money::format_usd_4;
use wanax_core::types::FactoryRun;

/// Fire `[run] notify_cmd` once if `run` is terminal. Loads config
/// itself so call sites need no plumbing; every failure is swallowed —
/// a notification must never fail a run.
pub fn fire_if_terminal(repo: &Path, run: &FactoryRun) {
    if !run.state.is_terminal() {
        return;
    }
    let cmd = load_merged_config(repo, &global_data_dir())
        .map(|c| c.file.run.notify_cmd)
        .unwrap_or_default();
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return;
    }
    let mut env = base_child_env();
    env.insert("WANAX_RUN_ID".into(), run.id.clone());
    env.insert("WANAX_STATE".into(), run.state.as_str().to_string());
    env.insert(
        "WANAX_SPENT_USD".into(),
        format_usd_4(run.spent_usd_micros).trim().to_string(),
    );
    env.insert(
        "WANAX_ADAPTER".into(),
        run.worker_adapter.as_str().to_string(),
    );
    if let Some(e) = &run.last_error {
        env.insert("WANAX_LAST_ERROR".into(), e.clone());
    }
    let _ = Command::new("/bin/sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(repo)
        .env_clear()
        .envs(&env)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn(); // detached: the caller must not wait on a hook
}
