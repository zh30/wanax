use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use wanax_core::error::{ErrorCode, WanaxError};

pub fn git_bin() -> Result<PathBuf, WanaxError> {
    which_os("git").or_else(|_| {
        let p = PathBuf::from("/usr/bin/git");
        if p.is_file() {
            Ok(p)
        } else {
            Err(WanaxError::from_code(ErrorCode::NotGit))
        }
    })
}

fn which_os(name: &str) -> Result<PathBuf, WanaxError> {
    if let Ok(p) = std::env::var("PATH") {
        for dir in p.split(':') {
            let cand = Path::new(dir).join(name);
            if cand.is_file() {
                return Ok(cand);
            }
        }
    }
    Err(WanaxError::from_code(ErrorCode::NotGit))
}

pub fn run_git(cwd: &Path, args: &[&str]) -> Result<Output, WanaxError> {
    let bin = which_os("git").or_else(|_| git_bin())?;
    let out = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| WanaxError::with_detail(ErrorCode::NotGit, e))?;
    Ok(out)
}

pub fn run_git_ok(cwd: &Path, args: &[&str]) -> Result<String, WanaxError> {
    let out = run_git(cwd, args)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(WanaxError::new(
            ErrorCode::Db,
            format!("git {} failed: {}", args.join(" "), stderr.trim()),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn is_git_repo(path: &Path) -> bool {
    run_git(path, &["rev-parse", "--is-inside-work-tree"])
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false)
}

pub fn require_git_repo(path: &Path) -> Result<(), WanaxError> {
    if is_git_repo(path) {
        Ok(())
    } else {
        Err(WanaxError::from_code(ErrorCode::NotGit))
    }
}

pub fn head_sha(repo: &Path) -> Result<String, WanaxError> {
    let sha = run_git_ok(repo, &["rev-parse", "HEAD"])?;
    if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(WanaxError::with_detail(
            ErrorCode::Db,
            "HEAD is not a 40-hex sha",
        ));
    }
    Ok(sha)
}

pub fn repo_root(path: &Path) -> Result<PathBuf, WanaxError> {
    let s = run_git_ok(path, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(s))
}

/// Uncommitted paths that are not under `.wanax/`.
pub fn dirty_non_wanax(repo: &Path) -> Result<Vec<String>, WanaxError> {
    Ok(status_paths(repo)?
        .into_iter()
        .filter(|path| !path.starts_with(".wanax/") && path != ".wanax")
        .collect())
}

pub fn create_branch(repo: &Path, name: &str, start_sha: &str) -> Result<(), WanaxError> {
    run_git_ok(repo, &["branch", name, start_sha])?;
    Ok(())
}

pub fn worktree_add_branch(repo: &Path, worktree: &Path, branch: &str) -> Result<(), WanaxError> {
    if let Some(parent) = worktree.parent() {
        fs::create_dir_all(parent).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    }
    run_git_ok(
        repo,
        &["worktree", "add", worktree.to_str().unwrap_or("."), branch],
    )?;
    Ok(())
}

pub fn worktree_add_detach(repo: &Path, worktree: &Path, sha: &str) -> Result<(), WanaxError> {
    if let Some(parent) = worktree.parent() {
        fs::create_dir_all(parent).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    }
    run_git_ok(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            worktree.to_str().unwrap_or("."),
            sha,
        ],
    )?;
    Ok(())
}

pub fn harden_inner_worktree(worktree: &Path) -> Result<(), WanaxError> {
    let _ = run_git_ok(worktree, &["config", "--local", "credential.helper", ""]);
    let _ = run_git_ok(worktree, &["config", "--local", "commit.gpgsign", "false"]);
    Ok(())
}

pub fn status_paths(worktree: &Path) -> Result<Vec<String>, WanaxError> {
    let mut paths = Vec::new();
    for args in [
        ["ls-files", "--others", "--exclude-standard"].as_slice(),
        ["diff", "--name-only"].as_slice(),
        ["diff", "--name-only", "--cached"].as_slice(),
    ] {
        let out = run_git_ok(worktree, args)?;
        for line in out.lines() {
            let path = line.trim();
            if !path.is_empty() {
                paths.push(path.to_string());
            }
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

pub fn add_files(worktree: &Path, files: &[String]) -> Result<(), WanaxError> {
    for f in files {
        run_git_ok(worktree, &["add", "--", f])?;
    }
    Ok(())
}

pub fn commit(worktree: &Path, message: &str) -> Result<String, WanaxError> {
    let out = run_git(
        worktree,
        &[
            "-c",
            "user.email=wanax@local",
            "-c",
            "user.name=wanax",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            message,
        ],
    )?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(WanaxError::new(
            ErrorCode::WorkerCrash,
            format!("git commit failed: {}", stderr.trim()),
        ));
    }
    head_sha(worktree)
}

pub fn diff_name_only(repo: &Path, a: &str, b: &str) -> Result<Vec<String>, WanaxError> {
    let spec = format!("{a}..{b}");
    let out = run_git_ok(repo, &["diff", "--name-only", &spec])?;
    Ok(out
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

pub fn diff_stat(repo: &Path, a: &str, b: &str) -> Result<String, WanaxError> {
    let spec = format!("{a}..{b}");
    let s = run_git_ok(repo, &["diff", "--stat", &spec])?;
    if s.len() > 4000 {
        Ok(s.chars().take(4000).collect())
    } else {
        Ok(s)
    }
}

pub fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> Result<bool, WanaxError> {
    let out = run_git(repo, &["merge-base", "--is-ancestor", ancestor, descendant])?;
    Ok(out.status.success())
}

/// Install a PATH-prepended git wrapper that denies push and protected-ref checkout.
pub fn install_git_wrapper(
    worktree: &Path,
    protected_refs: &[String],
) -> Result<PathBuf, WanaxError> {
    let dir = worktree.join(".wanax-bin");
    fs::create_dir_all(&dir).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    let real = which_os("git").or_else(|_| git_bin())?;
    let script = dir.join("git");
    let mut deny = String::new();
    for r in protected_refs {
        deny.push_str(&format!(
            "    if [ \"$a\" = \"{r}\" ]; then echo \"protected ref\" >&2; exit 1; fi\n"
        ));
    }
    let body = format!(
        r#"#!/bin/sh
real="{real}"
cmd="$1"
if [ "$cmd" = "push" ]; then
  echo "push denied" >&2
  exit 1
fi
if [ "$cmd" = "checkout" ] || [ "$cmd" = "switch" ]; then
  for a in "$@"; do
{deny}  done
fi
exec "$real" "$@"
"#,
        real = real.display(),
        deny = deny
    );
    fs::write(&script, body).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script)
            .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms)
            .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_repo() -> TempDir {
        let tmp = TempDir::new().unwrap();
        run_git_ok(tmp.path(), &["init"]).unwrap();
        run_git_ok(tmp.path(), &["config", "user.email", "t@t"]).unwrap();
        run_git_ok(tmp.path(), &["config", "user.name", "t"]).unwrap();
        fs::write(tmp.path().join("README"), "hi").unwrap();
        run_git_ok(tmp.path(), &["add", "README"]).unwrap();
        run_git_ok(tmp.path(), &["commit", "-m", "init"]).unwrap();
        tmp
    }

    #[test]
    fn dirty_ignores_wanax() {
        let repo = init_repo();
        fs::create_dir_all(repo.path().join(".wanax")).unwrap();
        fs::write(repo.path().join(".wanax/x"), "1").unwrap();
        fs::write(repo.path().join("dirty.txt"), "2").unwrap();
        let d = dirty_non_wanax(repo.path()).unwrap();
        assert_eq!(d, vec!["dirty.txt".to_string()]);
    }
}
