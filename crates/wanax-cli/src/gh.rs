use std::path::Path;
use std::process::Command;
use wanax_core::config::ResolvedConfig;
use wanax_core::error::{ErrorCode, WanaxError};

/// Create a GitHub PR after accept when configured. Runs in the outer process only.
pub fn maybe_create_github_pr(
    repo: &Path,
    inner_branch: &str,
    run_id: &str,
    cfg: &ResolvedConfig,
) -> Result<Option<String>, WanaxError> {
    let enabled = cfg.file.github.create_pr
        || std::env::var("WANAX_CREATE_PR").ok().as_deref() == Some("1");
    if !enabled {
        return Ok(None);
    }
    let has_token = std::env::var("GH_TOKEN")
        .ok()
        .filter(|s| !s.is_empty())
        .is_some()
        || std::env::var("GITHUB_TOKEN")
            .ok()
            .filter(|s| !s.is_empty())
            .is_some();
    if !has_token {
        tracing::warn!("github.create_pr enabled but GH_TOKEN missing in outer process");
        return Ok(None);
    }
    let title = format!("wanax: {run_id}");
    let body = format!(
        "Automated factory run `{run_id}`. Review branch `{inner_branch}` before merge."
    );
    let out = Command::new("gh")
        .args([
            "pr",
            "create",
            "--head",
            inner_branch,
            "--title",
            &title,
            "--body",
            &body,
        ])
        .current_dir(repo)
        .output()
        .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(WanaxError::new(
            ErrorCode::Db,
            format!("gh pr create failed: {}", stderr.trim()),
        ));
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(Some(url))
}
