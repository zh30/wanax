use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use wanax_core::envenv;
use wanax_core::error::{ErrorCode, WanaxError};
use wanax_core::sandbox::{self, SandboxSpec};
use wanax_core::types::Receipt;
use wanax_verify::run_test_command;

pub mod acp;

pub use wanax_core::envenv::{env_has_forbidden, FORBIDDEN_ENV};

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
    pub gitconfig: Option<PathBuf>,
    pub sandbox: Option<SandboxSpec>,
    /// `http://127.0.0.1:port` when worker egress is confined to the
    /// allowlist proxy.
    pub proxy_url: Option<String>,
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
    /// Real USD reported by the worker CLI, if it emits one (Claude).
    pub cost_usd_micros: Option<i64>,
    /// Token usage reported by the worker CLI, if it emits one (Codex).
    pub usage_in: Option<u64>,
    pub usage_out: Option<u64>,
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
    let mut env = envenv::base_child_env();
    if let Some(g) = &ctx.gitconfig {
        envenv::with_git_config(&mut env, g);
    }
    if let Some(extra) = &ctx.extra_path {
        envenv::with_prepend_path(&mut env, extra);
    }
    env.insert("WANAX_RUN_ID".into(), ctx.run_id.clone());
    env.insert("WANAX_WORK_UNIT_ID".into(), ctx.work_unit_id.clone());
    env.insert("WANAX_TEST_COMMAND".into(), ctx.test_command.clone());
    if let Some(p) = &ctx.proxy_url {
        for (k, v) in wanax_core::egress::proxy_env_from_url(p) {
            env.insert(k, v);
        }
    }
    for k in FORBIDDEN_ENV {
        env.remove(*k);
    }
    env
}

/// Spawn `bin args` through the optional OS sandbox, in its own process group
/// so timeouts and cancels can kill the whole tree, not just the leader.
pub(crate) fn spawn_confined(
    bin: &Path,
    args: &[String],
    ctx: &WorkerContext,
) -> Result<Command, WanaxError> {
    let (prog, argv) = match &ctx.sandbox {
        Some(spec) => sandbox::wrap(spec, bin, args)?,
        None => (bin.to_path_buf(), args.to_vec()),
    };
    let mut cmd = Command::new(prog);
    cmd.args(argv)
        .current_dir(&ctx.worktree)
        .env_clear()
        .envs(sanitized_env(ctx))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.as_std_mut().process_group(0);
    }
    Ok(cmd)
}

/// Signal every process in the group led by `pid` (negative pid = pgid).
/// `libc::killpg` directly — spawning `/bin/kill` under CI fork pressure
/// was flaky: if that spawn failed, the group was silently never killed.
pub fn kill_process_group(pid: u32, sig: &str) {
    #[cfg(unix)]
    {
        if pid == 0 {
            return;
        }
        let num = match sig {
            "-TERM" => libc::SIGTERM,
            "-KILL" => libc::SIGKILL,
            "-HUP" => libc::SIGHUP,
            "-INT" => libc::SIGINT,
            _ => libc::SIGKILL,
        };
        unsafe {
            libc::killpg(pid as libc::pid_t, num);
        }
        return;
    }
    #[allow(unreachable_code)]
    if pid > 0 {
        let _ = std::process::Command::new("kill")
            .args([sig, &format!("-{pid}")])
            .status();
    }
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
            let env = sanitized_env(ctx);
            let r = run_test_command(
                &ctx.worktree,
                &ctx.test_command,
                ctx.test_timeout_secs,
                &env,
                ctx.sandbox.as_ref(),
            )?;
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
            cost_usd_micros: None,
            usage_in: None,
            usage_out: None,
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
        let started = std::time::Instant::now();
        let args = vec!["--yolo".to_string(), "--message".to_string(), ctx.instruction.clone()];
        let mut cmd = spawn_confined(&bin, &args, ctx)?;
        let child = cmd.spawn().map_err(|e| {
            WanaxError::new(
                ErrorCode::AdapterMissing,
                format!("adapter binary not found: {}: {e}", self.bin),
            )
        })?;
        let pid = child.id().unwrap_or(0);
        let out = match tokio::time::timeout(
            std::time::Duration::from_secs(u64::from(ctx.timeout_secs)),
            child.wait_with_output(),
        )
        .await
        {
            Ok(v) => v.map_err(|e| WanaxError::with_detail(ErrorCode::WorkerCrash, e))?,
            Err(_) => {
                kill_process_group(pid, "-KILL");
                return Err(WanaxError::from_code(ErrorCode::WorkerTimeout));
            }
        };
        if !out.status.success() {
            return Err(WanaxError::from_code(ErrorCode::WorkerCrash));
        }
        let excerpt = tail_8000(&out.stdout, &out.stderr);
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
            cost_usd_micros: None,
            usage_in: None,
            usage_out: None,
        })
    }

    async fn status(&self, _handle: &WorkerHandle) -> Result<WorkerStatus, WanaxError> {
        Ok(WorkerStatus::Exited)
    }

    async fn cancel(&self, handle: &WorkerHandle) -> Result<(), WanaxError> {
        kill_process_group(handle.pid, "-TERM");
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
        let started = std::time::Instant::now();
        let mut cmd = spawn_confined(&bin, &self.args, ctx)?;
        cmd.env("WANAX_INSTRUCTION", &ctx.instruction);
        let child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => {
                WanaxError::new(
                    ErrorCode::AdapterMissing,
                    format!("adapter binary not found: {}: {e}", self.cmd),
                )
            }
            _ => WanaxError::with_detail(
                ErrorCode::WorkerCrash,
                format!("adapter spawn failed: {}: {e}", self.cmd),
            ),
        })?;
        let pid = child.id().unwrap_or(0);
        let out = match tokio::time::timeout(
            std::time::Duration::from_secs(u64::from(ctx.timeout_secs)),
            child.wait_with_output(),
        )
        .await
        {
            Ok(v) => v.map_err(|e| WanaxError::with_detail(ErrorCode::WorkerCrash, e))?,
            Err(_) => {
                kill_process_group(pid, "-KILL");
                return Err(WanaxError::from_code(ErrorCode::WorkerTimeout));
            }
        };
        if !out.status.success() {
            return Err(WanaxError::from_code(ErrorCode::WorkerCrash));
        }
        let excerpt = tail_8000(&out.stdout, &out.stderr);
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
            cost_usd_micros: None,
            usage_in: None,
            usage_out: None,
        })
    }

    async fn status(&self, _handle: &WorkerHandle) -> Result<WorkerStatus, WanaxError> {
        Ok(WorkerStatus::Exited)
    }

    async fn cancel(&self, handle: &WorkerHandle) -> Result<(), WanaxError> {
        kill_process_group(handle.pid, "-TERM");
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

const CLAUDE_FLAGS: &[&str] = &[
    "-p",
    "--dangerously-skip-permissions",
    "--output-format",
    "json",
];
const CODEX_FLAGS: &[&str] = &["exec", "--sandbox", "workspace-write", "--json", "-"];

pub struct ClaudeAdapter {
    pub bin: String,
    pub extra_args: Vec<String>,
}

impl ClaudeAdapter {
    pub fn new(bin: impl Into<String>, extra_args: Vec<String>) -> Self {
        Self {
            bin: bin.into(),
            extra_args,
        }
    }

    pub fn resolve_bin(&self) -> Result<PathBuf, WanaxError> {
        resolve_cmd_bin(&self.bin)
    }

    pub fn argv(&self) -> Vec<String> {
        let mut args: Vec<String> = CLAUDE_FLAGS.iter().map(|s| (*s).to_string()).collect();
        args.extend(self.extra_args.iter().cloned());
        args
    }
}

#[async_trait]
impl WorkerAdapter for ClaudeAdapter {
    async fn start(&self, ctx: &WorkerContext) -> Result<WorkerHandle, WanaxError> {
        let bin = self.resolve_bin()?;
        start_stdin_cli(bin, &self.argv(), ctx, "claude").await
    }

    async fn status(&self, _handle: &WorkerHandle) -> Result<WorkerStatus, WanaxError> {
        Ok(WorkerStatus::Exited)
    }

    async fn cancel(&self, handle: &WorkerHandle) -> Result<(), WanaxError> {
        cancel_pid(handle.pid)
    }

    async fn collect_receipt(
        &self,
        ctx: &WorkerContext,
        handle: &WorkerHandle,
    ) -> Result<Receipt, WanaxError> {
        receipt_from_handle(ctx, handle, "claude")
    }
}

pub struct CodexAdapter {
    pub bin: String,
    pub extra_args: Vec<String>,
}

impl CodexAdapter {
    pub fn new(bin: impl Into<String>, extra_args: Vec<String>) -> Self {
        Self {
            bin: bin.into(),
            extra_args,
        }
    }

    pub fn resolve_bin(&self) -> Result<PathBuf, WanaxError> {
        resolve_cmd_bin(&self.bin)
    }

    pub fn argv(&self) -> Vec<String> {
        let mut args: Vec<String> = CODEX_FLAGS.iter().map(|s| (*s).to_string()).collect();
        args.extend(self.extra_args.iter().cloned());
        args
    }
}

#[async_trait]
impl WorkerAdapter for CodexAdapter {
    async fn start(&self, ctx: &WorkerContext) -> Result<WorkerHandle, WanaxError> {
        let bin = self.resolve_bin()?;
        start_stdin_cli(bin, &self.argv(), ctx, "codex").await
    }

    async fn status(&self, _handle: &WorkerHandle) -> Result<WorkerStatus, WanaxError> {
        Ok(WorkerStatus::Exited)
    }

    async fn cancel(&self, handle: &WorkerHandle) -> Result<(), WanaxError> {
        cancel_pid(handle.pid)
    }

    async fn collect_receipt(
        &self,
        ctx: &WorkerContext,
        handle: &WorkerHandle,
    ) -> Result<Receipt, WanaxError> {
        receipt_from_handle(ctx, handle, "codex")
    }
}

fn cancel_pid(pid: u32) -> Result<(), WanaxError> {
    kill_process_group(pid, "-TERM");
    Ok(())
}

pub(crate) fn tail_8000(stdout: &[u8], stderr: &[u8]) -> String {
    let mut t = String::from_utf8_lossy(stdout).into_owned();
    t.push_str(&String::from_utf8_lossy(stderr));
    t.chars()
        .rev()
        .take(8000)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

/// Metrics parsed from a worker CLI's structured output.
#[derive(Debug, Clone, Default)]
struct CliMeter {
    cost_usd_micros: Option<i64>,
    usage_in: Option<u64>,
    usage_out: Option<u64>,
    turns: Option<u32>,
    /// Final assistant text to use as the excerpt.
    text: Option<String>,
    is_error: bool,
}

/// `claude -p --output-format json` emits one JSON object with cost/turns.
fn parse_claude_json(stdout: &str) -> Option<CliMeter> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    if v.get("is_error").is_none() && v.get("num_turns").is_none() {
        return None;
    }
    Some(CliMeter {
        cost_usd_micros: v
            .get("total_cost_usd")
            .and_then(|x| x.as_f64())
            .map(|usd| (usd * 1_000_000.0).round() as i64),
        usage_in: v
            .get("usage")
            .and_then(|u| u.get("input_tokens"))
            .and_then(|x| x.as_u64()),
        usage_out: v
            .get("usage")
            .and_then(|u| u.get("output_tokens"))
            .and_then(|x| x.as_u64()),
        turns: v
            .get("num_turns")
            .and_then(|x| x.as_u64())
            .map(|n| n as u32),
        text: v.get("result").and_then(|x| x.as_str()).map(String::from),
        is_error: v
            .get("is_error")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
    })
}

/// `codex exec --json` emits JSONL events; `turn.completed` carries usage.
fn parse_codex_jsonl(stdout: &str) -> CliMeter {
    let mut m = CliMeter::default();
    let mut turns = 0u32;
    let mut texts = Vec::new();
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("turn.completed") => {
                turns += 1;
                if let Some(u) = v.get("usage") {
                    m.usage_in = u.get("input_tokens").and_then(|x| x.as_u64()).or(m.usage_in);
                    m.usage_out = u.get("output_tokens").and_then(|x| x.as_u64()).or(m.usage_out);
                }
            }
            Some("item.completed")
                if v.pointer("/item/type").and_then(|t| t.as_str()) == Some("agent_message") =>
            {
                if let Some(t) = v.pointer("/item/text").and_then(|x| x.as_str()) {
                    texts.push(t.to_string());
                }
            }
            _ => {}
        }
    }
    if turns > 0 {
        m.turns = Some(turns);
    }
    if !texts.is_empty() {
        m.text = Some(texts.join("\n"));
    }
    m
}

pub(crate) fn receipt_from_handle(
    ctx: &WorkerContext,
    handle: &WorkerHandle,
    adapter: &str,
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
        adapter: adapter.into(),
        raw_artifact_path: handle.raw_artifact_path.clone(),
    })
}

async fn start_stdin_cli(
    bin: PathBuf,
    args: &[String],
    ctx: &WorkerContext,
    adapter: &str,
) -> Result<WorkerHandle, WanaxError> {
    use tokio::io::AsyncWriteExt;
    let started = std::time::Instant::now();
    let mut cmd = spawn_confined(&bin, args, ctx)?;
    cmd.env("WANAX_INSTRUCTION", &ctx.instruction);
    let mut child = cmd.spawn().map_err(|e| {
        WanaxError::new(
            ErrorCode::AdapterMissing,
            format!("adapter binary not found: {}: {e}", bin.display()),
        )
    })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(ctx.instruction.as_bytes())
            .await
            .map_err(|e| WanaxError::with_detail(ErrorCode::WorkerCrash, e))?;
    }
    let pid = child.id().unwrap_or(0);
    let out = match tokio::time::timeout(
        std::time::Duration::from_secs(u64::from(ctx.timeout_secs)),
        child.wait_with_output(),
    )
    .await
    {
        Ok(v) => v.map_err(|e| WanaxError::with_detail(ErrorCode::WorkerCrash, e))?,
        Err(_) => {
            kill_process_group(pid, "-KILL");
            return Err(WanaxError::from_code(ErrorCode::WorkerTimeout));
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let meter = match adapter {
        "claude" => parse_claude_json(&stdout).unwrap_or_default(),
        "codex" => parse_codex_jsonl(&stdout),
        _ => CliMeter::default(),
    };
    let excerpt = match meter.text {
        Some(ref t) if !t.is_empty() => {
            let mut t = t.clone();
            t.push_str(&String::from_utf8_lossy(&out.stderr));
            t.chars()
                .rev()
                .take(8000)
                .collect::<String>()
                .chars()
                .rev()
                .collect()
        }
        _ => tail_8000(&out.stdout, &out.stderr),
    };
    if meter.is_error || !out.status.success() {
        return Err(WanaxError::new(
            ErrorCode::WorkerCrash,
            excerpt.chars().take(2000).collect::<String>(),
        ));
    }
    Ok(WorkerHandle {
        pid,
        claimed_pass: out.status.success(),
        test_exit_code: out.status.code().unwrap_or(1),
        test_excerpt: excerpt,
        duration_ms: started.elapsed().as_millis() as u64,
        turns: meter.turns.unwrap_or(1).max(1),
        crashed: false,
        timed_out: false,
        raw_artifact_path: None,
        cost_usd_micros: meter.cost_usd_micros,
        usage_in: meter.usage_in,
        usage_out: meter.usage_out,
    })
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
            gitconfig: None,
            sandbox: None,
            proxy_url: None,
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
            gitconfig: None,
            sandbox: None,
            proxy_url: None,
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
            gitconfig: None,
            sandbox: None,
            proxy_url: None,
        };
        let err = adapter.start(&ctx).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::WorkerTimeout);
    }

    fn ctx_in(dir: &Path, instruction: &str) -> WorkerContext {
        WorkerContext {
            run_id: "wx_01AAAAAAAAAAAAAAAAAAAAAAAA".into(),
            work_unit_id: "wx_01BBBBBBBBBBBBBBBBBBBBBBBB".into(),
            test_command: "cargo test".into(),
            test_timeout_secs: 30,
            worktree: dir.to_path_buf(),
            instruction: instruction.into(),
            adapter_name: "claude".into(),
            extra_path: None,
            timeout_secs: 10,
            gitconfig: None,
            sandbox: None,
            proxy_url: None,
        }
    }

    #[tokio::test]
    async fn claude_adapter_pipes_instruction_on_stdin_not_argv() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("claude");
        std::fs::write(
            &script,
            "#!/bin/sh\ncat > stdin.txt\nprintf %s \"$*\" > args.txt\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let adapter = ClaudeAdapter::new(script.display().to_string(), Vec::new());
        let ctx = ctx_in(dir.path(), "secret task body");
        adapter.start(&ctx).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("stdin.txt")).unwrap(),
            "secret task body"
        );
        let args = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
        assert!(args.contains("-p"));
        assert!(args.contains("--dangerously-skip-permissions"));
        assert!(!args.contains("secret"));
    }

    #[tokio::test]
    async fn codex_adapter_uses_exec_sandbox_flags() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("codex");
        std::fs::write(
            &script,
            "#!/bin/sh\ncat > stdin.txt\nprintf %s \"$*\" > args.txt\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let adapter = CodexAdapter::new(script.display().to_string(), Vec::new());
        adapter.start(&ctx_in(dir.path(), "do the work")).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("stdin.txt")).unwrap(),
            "do the work"
        );
        let args = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
        assert!(args.contains("exec"));
        assert!(args.contains("workspace-write"));
        assert!(args.contains("--json"));
        assert!(!args.contains("do the work"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_whole_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("grandchild.pid");
        let script = dir.path().join("tree.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nsleep 60 &\necho $! > '{}'\nsleep 60\n",
                pidfile.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let adapter = CmdAdapter::new(script.display().to_string(), Vec::new());
        let mut ctx = ctx_in(dir.path(), "tree");
        ctx.adapter_name = "cmd".into();
        ctx.timeout_secs = 1;
        let err = adapter.start(&ctx).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::WorkerTimeout);
        let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_string();
        // Poll rather than fixed-sleep: `kill -0` also succeeds on a
        // zombie whose reaper hasn't cleaned up yet (common on loaded
        // CI boxes). Treat zombie state as dead.
        let mut alive = true;
        for _ in 0..40 {
            let status = std::process::Command::new("kill")
                .args(["-0", &pid])
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .map(|s| s.contains(") Z"))
                .unwrap_or(false);
            if !status || zombie {
                alive = false;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(!alive, "grandchild {pid} survived the timeout kill");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sandbox_denies_writes_outside_worktree() {
        if wanax_core::sandbox::detect_backend("auto") == wanax_core::sandbox::SandboxBackend::None {
            return; // no seatbelt/bwrap on this host
        }
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        // Escape target lives under $HOME (not TMPDIR — that IS writable).
        let escape = wanax_core::config::home_dir().join(".wanax-sb-escape-test");
        let _ = std::fs::remove_file(&escape);
        let script = dir.path().join("w.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ntouch '{}'\ntouch inside.txt\n",
                escape.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let spec = wanax_core::sandbox::resolve(
            "auto",
            vec![wt.clone()],
            &[],
            Some(dir.path().join("sandbox.sb")),
            wanax_core::sandbox::NetConfine::Open,
        )
        .unwrap();
        let adapter = CmdAdapter::new(script.display().to_string(), Vec::new());
        let mut ctx = ctx_in(&wt, "write");
        ctx.adapter_name = "cmd".into();
        ctx.sandbox = Some(spec);
        adapter.start(&ctx).await.unwrap();
        assert!(
            !escape.exists(),
            "sandboxed child wrote outside its worktree"
        );
        assert!(wt.join("inside.txt").is_file());
        let _ = std::fs::remove_file(&escape);
    }

    #[test]
    fn claude_json_metering_parsed() {
        let m = parse_claude_json(
            r#"{"is_error":false,"num_turns":3,"total_cost_usd":0.5,"result":"done"}"#,
        )
        .unwrap();
        assert_eq!(m.turns, Some(3));
        assert_eq!(m.cost_usd_micros, Some(500_000));
        assert_eq!(m.text.as_deref(), Some("done"));
        assert!(!m.is_error);
        // A bare text reply (old claude) degrades to no metering.
        assert!(parse_claude_json("not json").is_none());
    }

    #[test]
    fn codex_jsonl_metering_parsed() {
        let m = parse_codex_jsonl(
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"hi\"}}\n\
             {\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}\n",
        );
        assert_eq!(m.usage_in, Some(10));
        assert_eq!(m.usage_out, Some(5));
        assert_eq!(m.turns, Some(1));
        assert_eq!(m.text.as_deref(), Some("hi"));
    }

    #[tokio::test]
    async fn claude_is_error_maps_to_crash() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("claude");
        std::fs::write(
            &script,
            "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"is_error\":true,\"result\":\"refused\"}'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let adapter = ClaudeAdapter::new(script.display().to_string(), Vec::new());
        let err = adapter.start(&ctx_in(dir.path(), "x")).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::WorkerCrash);
    }
}
