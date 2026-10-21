pub mod peer;
pub mod plugin;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
pub use peer::{find_peer_overlap, peer_glob_sets_overlap};
pub use plugin::{run_verifier_plugins, PluginReport};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use wanax_core::error::{ErrorCode, WanaxError};
use wanax_core::sandbox::{self, SandboxSpec};

pub fn compile_globs(patterns: &[String]) -> Result<GlobSet, WanaxError> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        let g = GlobBuilder::new(p)
            .literal_separator(false)
            .build()
            .map_err(|e| WanaxError::with_detail(ErrorCode::ContractInvalid, e))?;
        b.add(g);
    }
    b.build()
        .map_err(|e| WanaxError::with_detail(ErrorCode::ContractInvalid, e))
}

/// True when `allowed_globs` can match files under `tests/` (integration tests).
/// `src/**` alone does not trigger this — unit tests inside `src/` are a separate hole.
pub fn allowed_globs_cover_binding_tests(patterns: &[String]) -> bool {
    if patterns.iter().any(|p| {
        p.split('/')
            .any(|seg| seg == "tests" || seg.eq_ignore_ascii_case("tests"))
    }) {
        return true;
    }
    match compile_globs(patterns) {
        Ok(set) => set.is_match("tests/foo.rs") || set.is_match("tests/a/b.rs"),
        Err(_) => false,
    }
}

pub fn is_factory_meta(path: &str) -> bool {
    path.starts_with(".wanax/runs/") || path.starts_with(".wanax/worktrees/")
}

#[derive(Debug, Clone)]
pub struct BoundaryReport {
    pub ok: bool,
    pub changed_files: Vec<String>,
    pub violating: Vec<String>,
}

pub fn check_boundaries(
    changed_files: &[String],
    allowed: &[String],
    forbidden: &[String],
) -> Result<BoundaryReport, WanaxError> {
    let allowed_set = compile_globs(allowed)?;
    let forbidden_set = if forbidden.is_empty() {
        None
    } else {
        Some(compile_globs(forbidden)?)
    };
    let mut violating = Vec::new();
    let considered: Vec<String> = changed_files
        .iter()
        .filter(|p| !is_factory_meta(p))
        .cloned()
        .collect();
    for path in &considered {
        let forbidden_hit = forbidden_set
            .as_ref()
            .map(|s| s.is_match(path))
            .unwrap_or(false);
        let allowed_hit = allowed_set.is_match(path);
        if forbidden_hit || !allowed_hit {
            violating.push(path.clone());
        }
    }
    Ok(BoundaryReport {
        ok: violating.is_empty(),
        changed_files: considered,
        violating,
    })
}

#[derive(Debug, Clone)]
pub struct TestRunResult {
    pub exit_code: i32,
    pub excerpt: String,
    pub cwd: PathBuf,
    pub timed_out: bool,
    pub duration_ms: u64,
}

/// Run `test_command` in `cwd` with a sanitized environment (never the wanax
/// process env — spawned test code can otherwise read `WANAX_*_API_KEY` and
/// exfiltrate it through stdout into tombstones and commander prompts).
/// Timeout → SIGKILL to the whole process group, exit_code 124.
pub fn run_test_command(
    cwd: &Path,
    test_command: &str,
    timeout_secs: u32,
    env: &HashMap<String, String>,
    sandbox_spec: Option<&SandboxSpec>,
) -> Result<TestRunResult, WanaxError> {
    let started = Instant::now();
    let args = vec!["-c".to_string(), test_command.to_string()];
    let (prog, argv) = match sandbox_spec {
        Some(spec) => sandbox::wrap(spec, Path::new("/bin/sh"), &args)?,
        None => (PathBuf::from("/bin/sh"), args),
    };
    let mut command = Command::new(prog);
    command
        .args(&argv)
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|e| WanaxError::with_detail(ErrorCode::WorkerCrash, e))?;
    let child_pid = child.id();

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| WanaxError::from_code(ErrorCode::WorkerCrash))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| WanaxError::from_code(ErrorCode::WorkerCrash))?;
    let stdout_h = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let stderr_h = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let timeout = Duration::from_secs(u64::from(timeout_secs));
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() > timeout {
                    kill_tree(child_pid);
                    let _ = child.wait();
                    let _ = stdout_h.join();
                    let _ = stderr_h.join();
                    return Ok(TestRunResult {
                        exit_code: 124,
                        excerpt: ErrorCode::OuterTestTimeout.as_str().to_string(),
                        cwd: cwd.to_path_buf(),
                        timed_out: true,
                        duration_ms: started.elapsed().as_millis() as u64,
                    });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(WanaxError::with_detail(ErrorCode::WorkerCrash, e)),
        }
    };

    let out = stdout_h.join().unwrap_or_default();
    let err = stderr_h.join().unwrap_or_default();
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&out));
    text.push_str(&String::from_utf8_lossy(&err));
    Ok(TestRunResult {
        exit_code: status.code().unwrap_or(1),
        excerpt: tail_chars(&text, 8000),
        cwd: cwd.to_path_buf(),
        timed_out: false,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

/// Kill the whole process group led by `pid`, falling back to the single
/// process. Children were spawned with `process_group(0)` so pgid == pid.
fn kill_tree(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-KILL", &format!("-{pid}")])
            .status();
    }
    let _ = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status();
}

fn tail_chars(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        s.chars().skip(count - max).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_src_rejects_cargo_toml() {
        let r = check_boundaries(
            &["src/foo.rs".into(), "Cargo.toml".into()],
            &["src/**".into()],
            &["**/.env".into()],
        )
        .unwrap();
        assert!(!r.ok);
        assert_eq!(r.violating, vec!["Cargo.toml".to_string()]);
    }

    #[test]
    fn wanax_meta_paths_ignored() {
        let r = check_boundaries(
            &[
                "src/foo.rs".into(),
                ".wanax/runs/wx_x/envelope.json".into(),
                ".wanax/worktrees/x/src/lib.rs".into(),
            ],
            &["src/**".into()],
            &[],
        )
        .unwrap();
        assert!(r.ok);
    }

    #[test]
    fn src_globs_do_not_cover_binding_tests() {
        assert!(!allowed_globs_cover_binding_tests(&["src/**".into()]));
    }

    #[test]
    fn tests_globs_and_star_cover_binding_tests() {
        assert!(allowed_globs_cover_binding_tests(&["tests/**".into()]));
        assert!(allowed_globs_cover_binding_tests(&[
            "src/**".into(),
            "tests/**".into()
        ]));
        assert!(allowed_globs_cover_binding_tests(&["**/*".into()]));
        assert!(allowed_globs_cover_binding_tests(&["**/*.rs".into()]));
        assert!(allowed_globs_cover_binding_tests(&[
            "crates/foo/tests/**".into()
        ]));
    }

    #[test]
    fn forbidden_env_fails() {
        let r = check_boundaries(
            &["src/.env".into()],
            &["src/**".into()],
            &["**/.env".into()],
        )
        .unwrap();
        assert!(!r.ok);
    }

    /// A spawned test harness must not see the wanax process's secrets.
    #[test]
    fn test_env_does_not_leak_parent_vars() {
        std::env::set_var("WANAX_COMMANDER_API_KEY", "sk-leak");
        let dir = tempfile::tempdir().unwrap();
        let env = wanax_core::envenv::base_child_env();
        let r = run_test_command(
            dir.path(),
            "printf %s \"$WANAX_COMMANDER_API_KEY\" > leak.txt",
            30,
            &env,
            None,
        )
        .unwrap();
        std::env::remove_var("WANAX_COMMANDER_API_KEY");
        assert_eq!(r.exit_code, 0, "{}", r.excerpt);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("leak.txt")).unwrap(),
            ""
        );
    }
}
