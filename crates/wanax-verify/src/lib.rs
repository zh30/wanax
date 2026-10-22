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

/// `must_have:` files named by criteria that the diff never touched.
/// Each entry is `CC-NN: pattern`. Exact paths and globs both work;
/// a pattern is satisfied when at least one changed file matches it.
pub fn missing_must_have(
    criteria: &[wanax_core::types::CompletionCriterion],
    changed: &[String],
) -> Vec<String> {
    let mut missing = Vec::new();
    for cc in criteria {
        for pat in &cc.must_have_files {
            let hit = changed.iter().any(|f| {
                f == pat
                    || globset::Glob::new(pat)
                        .map(|g| g.compile_matcher().is_match(f))
                        .unwrap_or(false)
            });
            if !hit {
                missing.push(format!("{}: {}", cc.id, pat));
            }
        }
    }
    missing
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

// ---- content-level secret scan -------------------------------------------
//
// Path boundaries (forbidden_globs) can't see CONTENT: a worker writing an
// API key into src/config.rs passes every glob check. Scan the added lines
// of the accepted diff for high-signal secret shapes so the accept gate
// blocks the leak before it lands in git history.

#[derive(Debug, Clone)]
pub struct SecretFinding {
    pub path: String,
    pub line: u32,
    /// Pattern class label only — the matched secret itself is never stored.
    pub kind: &'static str,
}

const SECRET_RULES: &[(&str, &str)] = &[
    ("aws-access-key", r"AKIA[0-9A-Z]{16}"),
    (
        "private-key-block",
        r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP )?PRIVATE KEY",
    ),
    (
        "github-token",
        r"gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{22,}",
    ),
    ("openai-key", r"sk-[A-Za-z0-9_-]{20,}"),
    (
        "stripe-live-key",
        r"(?:sk|rk)_live_[0-9a-zA-Z]{16,}|whsec_[A-Za-z0-9]{16,}",
    ),
    ("slack-token", r"xox[baprs]-[0-9A-Za-z-]{10,}"),
    ("google-api-key", r"AIza[0-9A-Za-z_-]{35}"),
    (
        "jwt",
        r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}",
    ),
    (
        "hardcoded-credential",
        r#"(?i)(?:api[_-]?key|secret[_-]?key|password|passwd|access[_-]?token|auth[_-]?token)\s*[:=]\s*["'][0-9A-Za-z_\-+/=.]{16,}["']"#,
    ),
];

fn secret_patterns() -> &'static [(regex::Regex, &'static str)] {
    use std::sync::OnceLock;
    static PATTERNS: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        SECRET_RULES
            .iter()
            .map(|(kind, pat)| {
                (
                    regex::Regex::new(pat).expect("secret regex compiles"),
                    *kind,
                )
            })
            .collect()
    })
}

/// Scan the ADDED lines of a unified diff for high-signal secret shapes.
/// Returns findings with file + new-file line numbers — never the secret.
pub fn scan_diff_added(diff: &str) -> Vec<SecretFinding> {
    let patterns = secret_patterns();
    let mut out = Vec::new();
    let mut path = String::new();
    let mut new_line: u32 = 0;
    let mut in_hunk = false;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            path = rest
                .trim()
                .strip_prefix("b/")
                .unwrap_or(rest.trim())
                .to_string();
            in_hunk = false;
        } else if let Some(h) = line.strip_prefix("@@ ") {
            // @@ -a,b +c,d @@ → new-file lines start at c
            if let Some(plus) = h.split('+').nth(1) {
                new_line = plus
                    .split([',', ' '])
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
            }
            in_hunk = true;
        } else if in_hunk {
            if let Some(added) = line.strip_prefix('+') {
                for (re, kind) in patterns {
                    if re.is_match(added) {
                        out.push(SecretFinding {
                            path: path.clone(),
                            line: new_line,
                            kind,
                        });
                        break;
                    }
                }
                new_line += 1;
            } else if line.starts_with('-') || line.starts_with('\\') {
                // removed context or "no newline" marker: no new-line advance
            } else {
                new_line += 1; // context line
            }
        }
    }
    out
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

    #[test]
    fn scan_finds_keys_only_in_added_lines() {
        let diff = "\
diff --git a/src/cfg.rs b/src/cfg.rs
index 111..222 100644
--- a/src/cfg.rs
+++ b/src/cfg.rs
@@ -10,3 +10,4 @@
 let a = 1;
-let old = 2;
+let key = \"sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ12\";
+let note = \"ok\";
";
        let f = scan_diff_added(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, "openai-key");
        assert_eq!(f[0].path, "src/cfg.rs");
        assert_eq!(f[0].line, 11);
    }

    #[test]
    fn scan_ignores_removed_lines_and_clean_code() {
        let diff = "\
--- a/src/cfg.rs
+++ b/src/cfg.rs
@@ -1,3 +1,3 @@
-const GONE: &str = \"AKIAIOSFODNN7EXAMPLE\";
+pub fn add(a: i32, b: i32) -> i32 { a + b }
+#[test] fn t() { assert_eq!(add(1,2),3); }
";
        assert!(scan_diff_added(diff).is_empty());
    }

    #[test]
    fn scan_flags_private_key_and_hardcoded_creds() {
        let diff = "\
+++ b/keys.txt
@@ -0,0 +1,3 @@
+-----BEGIN PRIVATE KEY-----
+aws = \"AKIAIOSFODNN7EXAMPLE\"
+password = \"hunter2-hunter2-hunter2\"
";
        let kinds: Vec<_> = scan_diff_added(diff).iter().map(|f| f.kind).collect();
        assert!(kinds.contains(&"private-key-block"));
        assert!(kinds.contains(&"aws-access-key"));
        assert!(kinds.contains(&"hardcoded-credential"));
    }

    #[test]
    fn scan_tracks_lines_across_multiple_hunks_and_files() {
        let diff = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,2 +1,2 @@
 let a = 1;
+let key = \"AKIAIOSFODNN7EXAMPLE\";
@@ -40,1 +40,2 @@
 fn old() {}
+let k2 = \"ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij\";
diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -1,1 +1,2 @@
 x
+password = 'totally-secret-16chars'
";
        let f = scan_diff_added(diff);
        assert_eq!(f.len(), 3);
        assert_eq!(
            (f[0].path.as_str(), f[0].line, f[0].kind),
            ("a.rs", 2, "aws-access-key")
        );
        assert_eq!(
            (f[1].path.as_str(), f[1].line, f[1].kind),
            ("a.rs", 41, "github-token")
        );
        assert_eq!(
            (f[2].path.as_str(), f[2].kind),
            ("b.rs", "hardcoded-credential")
        );
    }

    #[test]
    fn scan_handles_new_deleted_and_no_newline_files() {
        // new file: --- /dev/null ; deleted file: +++ /dev/null (no adds count)
        let diff = "\
diff --git a/old.rs b/old.rs
--- a/old.rs
+++ /dev/null
@@ -1,1 +0,0 @@
-secret = \"AKIAIOSFODNN7EXAMPLE\"
diff --git a/new.rs b/new.rs
--- /dev/null
+++ b/new.rs
@@ -0,0 +1,2 @@
+const K: &str = \"AKIAIOSFODNN7EXAMPLE\";
+\\ No newline at end of file
";
        let f = scan_diff_added(diff);
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].path.as_str(), f[0].line), ("new.rs", 1));
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
