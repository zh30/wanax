pub mod peer;
pub mod plugin;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
pub use peer::{find_peer_overlap, peer_glob_sets_overlap};
pub use plugin::{run_verifier_plugins, PluginReport};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use wanax_core::error::{ErrorCode, WanaxError};

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

/// Run `test_command` in `cwd`. Timeout → exit_code 124.
pub fn run_test_command(
    cwd: &Path,
    test_command: &str,
    timeout_secs: u32,
) -> Result<TestRunResult, WanaxError> {
    let started = Instant::now();
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(test_command)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .spawn()
        .map_err(|e| WanaxError::with_detail(ErrorCode::WorkerCrash, e))?;

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
                    let _ = child.kill();
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
}
