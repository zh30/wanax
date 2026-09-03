use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use wanax_core::error::{ErrorCode, WanaxError};
use wanax_core::types::Receipt;
use wanax_verify::run_test_command;

const FORBIDDEN_ENV: &[&str] = &[
    "GIT_ASKPASS",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "WANAX_COMMANDER_API_KEY",
    "WANAX_INNER_API_KEY",
];

#[derive(Debug, Clone)]
pub struct WorkerContext {
    pub run_id: String,
    pub work_unit_id: String,
    pub test_command: String,
    pub test_timeout_secs: u32,
    pub worktree: PathBuf,
    pub instruction: String,
    pub adapter_name: String,
    pub extra_path: Option<PathBuf>,
    pub timeout_secs: u32,
}

#[derive(Debug, Clone)]
pub struct WorkerHandle {
    pub pid: u32,
    pub claimed_pass: bool,
    pub test_exit_code: i32,
    pub test_excerpt: String,
    pub duration_ms: u64,
    pub turns: u32,
    pub crashed: bool,
    pub timed_out: bool,
    pub raw_artifact_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerStatus {
    Running,
    Exited,
}

#[async_trait]
pub trait WorkerAdapter: Send + Sync {
    async fn start(&self, ctx: &WorkerContext) -> Result<WorkerHandle, WanaxError>;
    async fn status(&self, handle: &WorkerHandle) -> Result<WorkerStatus, WanaxError>;
    async fn cancel(&self, handle: &WorkerHandle) -> Result<(), WanaxError>;
    async fn collect_receipt(
        &self,
        ctx: &WorkerContext,
        handle: &WorkerHandle,
    ) -> Result<Receipt, WanaxError>;
}

pub fn sanitized_env(ctx: &WorkerContext) -> HashMap<String, String> {
    let mut env = HashMap::new();
    for key in [
        "PATH", "HOME", "USER", "TMPDIR", "LANG", "LC_ALL", "TERM", "SHELL",
    ] {
        if let Ok(v) = std::env::var(key) {
            env.insert(key.to_string(), v);
        }
    }
    if let Some(extra) = &ctx.extra_path {
        let path = match env.get("PATH") {
            Some(p) => format!("{}:{p}", extra.display()),
            None => extra.display().to_string(),
        };
        env.insert("PATH".into(), path);
    }
    env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
    env.insert("WANAX_RUN_ID".into(), ctx.run_id.clone());
    env.insert("WANAX_WORK_UNIT_ID".into(), ctx.work_unit_id.clone());
    env.insert("WANAX_TEST_COMMAND".into(), ctx.test_command.clone());
    for k in FORBIDDEN_ENV {
        env.remove(*k);
    }
    env
}

pub fn env_has_forbidden(env: &HashMap<String, String>) -> bool {
    FORBIDDEN_ENV.iter().any(|k| env.contains_key(*k))
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FakeSpec {
    #[serde(default = "one")]
    pub turns: u32,
    #[serde(default)]
    pub attempt_push: bool,
    #[serde(default)]
    pub crash: bool,
    #[serde(default)]
    pub timeout: bool,
    #[serde(default)]
    pub sleep_ms: u64,
    pub claimed_pass: Option<bool>,
    #[serde(default)]
    pub run_tests: bool,
    #[serde(default)]
    pub writes: Vec<FakeWrite>,
    #[serde(default)]
    pub attempts: Vec<FakeAttempt>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FakeAttempt {
    #[serde(default)]
    pub writes: Vec<FakeWrite>,
    pub claimed_pass: Option<bool>,
    #[serde(default)]
    pub crash: bool,
    #[serde(default)]
    pub attempt_push: bool,
    #[serde(default = "one")]
    pub turns: u32,
    #[serde(default)]
    pub run_tests: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FakeWrite {
    pub path: String,
    pub content: String,
}

fn one() -> u32 {
    1
}

pub fn load_fake_spec(worktree_or_repo: &Path) -> FakeSpec {
    if let Ok(p) = std::env::var("WANAX_FAKE_SPEC") {
        if let Ok(text) = std::fs::read_to_string(&p) {
            if let Ok(spec) = toml::from_str(&text) {
                return spec;
            }
        }
    }
    let candidates = [
        worktree_or_repo.join(".wanax/fake.toml"),
        worktree_or_repo.join("fake.toml"),
    ];
    for c in candidates {
        if let Ok(text) = std::fs::read_to_string(c) {
            if let Ok(spec) = toml::from_str(&text) {
                return spec;
            }
        }
    }
    FakeSpec::default()
}

pub struct FakeAdapter {
    pub rework_count: u32,
    pub goal_iter: u32,
    pub spec_path: Option<PathBuf>,
}

impl FakeAdapter {
    pub fn new(rework_count: u32) -> Self {
        Self {
            rework_count,
            goal_iter: 1,
            spec_path: None,
        }
    }

    fn attempt_index(&self) -> usize {
        self.rework_count
            .saturating_add(self.goal_iter.saturating_sub(1)) as usize
    }
}

#[async_trait]
impl WorkerAdapter for FakeAdapter {
    async fn start(&self, ctx: &WorkerContext) -> Result<WorkerHandle, WanaxError> {
        let spec = if let Some(p) = &self.spec_path {
            std::fs::read_to_string(p)
                .ok()
                .and_then(|t| toml::from_str(&t).ok())
                .unwrap_or_else(|| load_fake_spec(&ctx.worktree))
        } else {
            load_fake_spec(&ctx.worktree)
        };
        let attempt = spec.attempts.get(self.attempt_index()).cloned();
        let writes = attempt
            .as_ref()
            .map(|a| a.writes.clone())
            .unwrap_or(spec.writes.clone());
        let crash = attempt.as_ref().map(|a| a.crash).unwrap_or(spec.crash);
        let attempt_push = attempt
            .as_ref()
            .map(|a| a.attempt_push)
            .unwrap_or(spec.attempt_push);
        let turns = attempt.as_ref().map(|a| a.turns).unwrap_or(spec.turns);
        let run_tests = attempt
            .as_ref()
            .map(|a| a.run_tests)
            .unwrap_or(spec.run_tests);
        let claimed_override = attempt
            .as_ref()
            .and_then(|a| a.claimed_pass)
            .or(spec.claimed_pass);

        if spec.timeout {
            tokio::time::sleep(std::time::Duration::from_secs(
                u64::from(ctx.timeout_secs) + 1,
            ))
            .await;
            return Err(WanaxError::from_code(ErrorCode::WorkerTimeout));
        }
        if spec.sleep_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(spec.sleep_ms)).await;
        }
        if crash {
            return Err(WanaxError::from_code(ErrorCode::WorkerCrash));
        }
        for w in &writes {
            let path = ctx.worktree.join(&w.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| WanaxError::with_detail(ErrorCode::WorkerCrash, e))?;
            }
            std::fs::write(&path, &w.content)
                .map_err(|e| WanaxError::with_detail(ErrorCode::WorkerCrash, e))?;
        }
        if attempt_push {
            let env = sanitized_env(ctx);
            let mut cmd = Command::new("git");
            cmd.arg("push")
                .current_dir(&ctx.worktree)
                .envs(&env)
                .env_remove("GIT_ASKPASS")
                .env_remove("GH_TOKEN")
                .env_remove("SSH_AUTH_SOCK")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let out = cmd
                .output()
                .await
                .map_err(|e| WanaxError::with_detail(ErrorCode::PushAttempt, e))?;
            if out.status.success() {
                return Err(WanaxError::new(
                    ErrorCode::PushAttempt,
                    "inner git push succeeded (security defect)",
                ));
            }
        }

        let mut test_exit = 0;
        let mut excerpt = String::new();
        let mut duration_ms = 0;
        if run_tests {
            let r = run_test_command(&ctx.worktree, &ctx.test_command, ctx.test_timeout_secs)?;
            test_exit = r.exit_code;
            excerpt = r.excerpt;
            duration_ms = r.duration_ms;
        }
        let claimed_pass = claimed_override.unwrap_or(test_exit == 0);
        Ok(WorkerHandle {
            pid: std::process::id(),
            claimed_pass,
            test_exit_code: test_exit,
            test_excerpt: excerpt,
            duration_ms,
            turns: turns.max(1),
            crashed: false,
            timed_out: false,
            raw_artifact_path: None,
        })
    }

    async fn status(&self, _handle: &WorkerHandle) -> Result<WorkerStatus, WanaxError> {
        Ok(WorkerStatus::Exited)
    }

    async fn cancel(&self, _handle: &WorkerHandle) -> Result<(), WanaxError> {
        Ok(())
    }

    async fn collect_receipt(
        &self,
        ctx: &WorkerContext,
        handle: &WorkerHandle,
    ) -> Result<Receipt, WanaxError> {
        Ok(Receipt {
            id: wanax_core::new_id(),
            work_unit_id: ctx.work_unit_id.clone(),
            changed_files: Vec::new(),
            diffstat: String::new(),
            commit_sha: String::new(),
            test_command: ctx.test_command.clone(),
            test_exit_code: handle.test_exit_code,
            test_excerpt: handle.test_excerpt.clone(),
            claimed_pass: handle.claimed_pass,
            duration_ms: handle.duration_ms,
            adapter: ctx.adapter_name.clone(),
            raw_artifact_path: handle.raw_artifact_path.clone(),
        })
    }
}

pub struct OctoscodeAdapter {
    pub bin: String,
}

impl OctoscodeAdapter {
    pub fn new(bin: impl Into<String>) -> Self {
        Self { bin: bin.into() }
    }

    pub fn resolve_bin(&self) -> Result<PathBuf, WanaxError> {
        which::which(&self.bin).map_err(|_| {
            WanaxError::new(
                ErrorCode::AdapterMissing,
                format!("adapter binary not found: {}", self.bin),
            )
        })
    }

    pub fn has_yolo_flag(&self) -> Result<bool, WanaxError> {
        let bin = self.resolve_bin()?;
        let out = std::process::Command::new(&bin)
            .arg("--help")
            .output()
            .map_err(|_| {
                WanaxError::new(
                    ErrorCode::AdapterMissing,
                    format!("adapter binary not found: {}", self.bin),
                )
            })?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(text.contains("yolo"))
    }
}

#[async_trait]
impl WorkerAdapter for OctoscodeAdapter {
    async fn start(&self, ctx: &WorkerContext) -> Result<WorkerHandle, WanaxError> {
        let bin = self.resolve_bin()?;
        if !self.has_yolo_flag()? {
            return Err(WanaxError::new(
                ErrorCode::AdapterMissing,
                format!(
                    "adapter binary not found: {} (--yolo missing; [NEEDS CLARIFICATION])",
                    self.bin
                ),
            ));
        }
        let env = sanitized_env(ctx);
        debug_assert!(!env_has_forbidden(&env));
        let started = std::time::Instant::now();
        let mut cmd = Command::new(&bin);
        cmd.arg("--yolo")
            .arg("--message")
            .arg(&ctx.instruction)
            .current_dir(&ctx.worktree)
            .env_clear()
            .envs(&env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = cmd.spawn().map_err(|e| {
            WanaxError::new(
                ErrorCode::AdapterMissing,
                format!("adapter binary not found: {}: {e}", self.bin),
            )
        })?;
        let pid = child.id().unwrap_or(0);
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(u64::from(ctx.timeout_secs)),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| WanaxError::from_code(ErrorCode::WorkerTimeout))?
        .map_err(|e| WanaxError::with_detail(ErrorCode::WorkerCrash, e))?;
        if !out.status.success() {
            return Err(WanaxError::from_code(ErrorCode::WorkerCrash));
        }
        let excerpt = {
            let mut t = String::from_utf8_lossy(&out.stdout).into_owned();
            t.push_str(&String::from_utf8_lossy(&out.stderr));
            t.chars()
                .rev()
                .take(8000)
                .collect::<String>()
                .chars()
                .rev()
                .collect()
        };
        Ok(WorkerHandle {
            pid,
            claimed_pass: out.status.success(),
            test_exit_code: out.status.code().unwrap_or(1),
            test_excerpt: excerpt,
            duration_ms: started.elapsed().as_millis() as u64,
            turns: 1,
            crashed: false,
            timed_out: false,
            raw_artifact_path: None,
        })
    }

    async fn status(&self, _handle: &WorkerHandle) -> Result<WorkerStatus, WanaxError> {
        Ok(WorkerStatus::Exited)
    }

    async fn cancel(&self, handle: &WorkerHandle) -> Result<(), WanaxError> {
        if handle.pid > 0 {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &handle.pid.to_string()])
                .status();
        }
        Ok(())
    }

    async fn collect_receipt(
        &self,
        ctx: &WorkerContext,
        handle: &WorkerHandle,
    ) -> Result<Receipt, WanaxError> {
        Ok(Receipt {
            id: wanax_core::new_id(),
            work_unit_id: ctx.work_unit_id.clone(),
            changed_files: Vec::new(),
            diffstat: String::new(),
            commit_sha: String::new(),
            test_command: ctx.test_command.clone(),
            test_exit_code: handle.test_exit_code,
            test_excerpt: handle.test_excerpt.clone(),
            claimed_pass: handle.claimed_pass,
            duration_ms: handle.duration_ms,
            adapter: "octoscode".into(),
            raw_artifact_path: handle.raw_artifact_path.clone(),
        })
    }
}

pub fn resolve_cmd_bin(cmd: &str) -> Result<PathBuf, WanaxError> {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return Err(WanaxError::new(
            ErrorCode::AdapterMissing,
            "adapter binary not found: cmd",
        ));
    }
    let p = Path::new(trimmed);
    if p.is_file() {
        return Ok(p.to_path_buf());
    }
    which::which(trimmed).map_err(|_| {
        WanaxError::new(
            ErrorCode::AdapterMissing,
            format!("adapter binary not found: {trimmed}"),
        )
    })
}

pub struct CmdAdapter {
    pub cmd: String,
    pub args: Vec<String>,
}

impl CmdAdapter {
    pub fn new(cmd: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            cmd: cmd.into(),
            args,
        }
    }

    pub fn resolve_bin(&self) -> Result<PathBuf, WanaxError> {
        resolve_cmd_bin(&self.cmd)
    }
}

#[async_trait]
impl WorkerAdapter for CmdAdapter {
    async fn start(&self, ctx: &WorkerContext) -> Result<WorkerHandle, WanaxError> {
        let bin = self.resolve_bin()?;
        let mut env = sanitized_env(ctx);
        env.insert("WANAX_INSTRUCTION".into(), ctx.instruction.clone());
        debug_assert!(!env_has_forbidden(&env));
        let started = std::time::Instant::now();
        let mut cmd = Command::new(&bin);
        cmd.args(&self.args)
            .current_dir(&ctx.worktree)
            .env_clear()
            .envs(&env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = cmd.spawn().map_err(|e| {
            WanaxError::new(
                ErrorCode::AdapterMissing,
                format!("adapter binary not found: {}: {e}", self.cmd),
            )
        })?;
        let pid = child.id().unwrap_or(0);
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(u64::from(ctx.timeout_secs)),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| WanaxError::from_code(ErrorCode::WorkerTimeout))?
        .map_err(|e| WanaxError::with_detail(ErrorCode::WorkerCrash, e))?;
        if !out.status.success() {
            return Err(WanaxError::from_code(ErrorCode::WorkerCrash));
        }
        let excerpt = {
            let mut t = String::from_utf8_lossy(&out.stdout).into_owned();
            t.push_str(&String::from_utf8_lossy(&out.stderr));
            t.chars()
                .rev()
                .take(8000)
                .collect::<String>()
                .chars()
                .rev()
                .collect()
        };
        Ok(WorkerHandle {
            pid,
            claimed_pass: out.status.success(),
            test_exit_code: out.status.code().unwrap_or(1),
            test_excerpt: excerpt,
            duration_ms: started.elapsed().as_millis() as u64,
            turns: 1,
            crashed: false,
            timed_out: false,
            raw_artifact_path: None,
        })
    }

    async fn status(&self, _handle: &WorkerHandle) -> Result<WorkerStatus, WanaxError> {
        Ok(WorkerStatus::Exited)
    }

    async fn cancel(&self, handle: &WorkerHandle) -> Result<(), WanaxError> {
        if handle.pid > 0 {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &handle.pid.to_string()])
                .status();
        }
        Ok(())
    }

    async fn collect_receipt(
        &self,
        ctx: &WorkerContext,
        handle: &WorkerHandle,
    ) -> Result<Receipt, WanaxError> {
        Ok(Receipt {
            id: wanax_core::new_id(),
            work_unit_id: ctx.work_unit_id.clone(),
            changed_files: Vec::new(),
            diffstat: String::new(),
            commit_sha: String::new(),
            test_command: ctx.test_command.clone(),
            test_exit_code: handle.test_exit_code,
            test_excerpt: handle.test_excerpt.clone(),
            claimed_pass: handle.claimed_pass,
            duration_ms: handle.duration_ms,
            adapter: "cmd".into(),
            raw_artifact_path: handle.raw_artifact_path.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitized_env_omits_tokens() {
        let ctx = WorkerContext {
            run_id: "wx_01AAAAAAAAAAAAAAAAAAAAAAAA".into(),
            work_unit_id: "wx_01BBBBBBBBBBBBBBBBBBBBBBBB".into(),
            test_command: "cargo test".into(),
            test_timeout_secs: 30,
            worktree: PathBuf::from("/tmp"),
            instruction: "do".into(),
            adapter_name: "fake".into(),
            extra_path: None,
            timeout_secs: 30,
        };
        let env = sanitized_env(&ctx);
        assert!(!env_has_forbidden(&env));
        assert_eq!(
            env.get("WANAX_RUN_ID").map(String::as_str),
            Some(ctx.run_id.as_str())
        );
        assert!(!env.contains_key("GH_TOKEN"));
        assert!(!env.contains_key("SSH_AUTH_SOCK"));
        assert!(!env.contains_key("GIT_ASKPASS"));
    }

    #[test]
    fn resolve_cmd_bin_rejects_empty() {
        let err = resolve_cmd_bin("").unwrap_err();
        assert_eq!(err.code, ErrorCode::AdapterMissing);
    }

    #[tokio::test]
    async fn cmd_adapter_passes_instruction_via_env_only() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("worker.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf %s \"$WANAX_INSTRUCTION\" > out.txt\nprintf %s \"$*\" > args.txt\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let adapter = CmdAdapter::new(script.display().to_string(), vec!["--quiet".into()]);
        let ctx = WorkerContext {
            run_id: "wx_01AAAAAAAAAAAAAAAAAAAAAAAA".into(),
            work_unit_id: "wx_01BBBBBBBBBBBBBBBBBBBBBBBB".into(),
            test_command: "cargo test".into(),
            test_timeout_secs: 30,
            worktree: dir.path().to_path_buf(),
            instruction: "implement add without leaking".into(),
            adapter_name: "cmd".into(),
            extra_path: None,
            timeout_secs: 10,
        };
        adapter.start(&ctx).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("out.txt")).unwrap(),
            "implement add without leaking"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("args.txt")).unwrap(),
            "--quiet"
        );
    }

    #[tokio::test]
    async fn cmd_adapter_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("sleep.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 5\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let adapter = CmdAdapter::new(script.display().to_string(), Vec::new());
        let ctx = WorkerContext {
            run_id: "wx_01AAAAAAAAAAAAAAAAAAAAAAAA".into(),
            work_unit_id: "wx_01BBBBBBBBBBBBBBBBBBBBBBBB".into(),
            test_command: "cargo test".into(),
            test_timeout_secs: 30,
            worktree: dir.path().to_path_buf(),
            instruction: "sleep".into(),
            adapter_name: "cmd".into(),
            extra_path: None,
            timeout_secs: 1,
        };
        let err = adapter.start(&ctx).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::WorkerTimeout);
    }
}
