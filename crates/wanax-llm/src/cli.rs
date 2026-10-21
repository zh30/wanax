use crate::http::{verdict_user, DISPATCH_SYSTEM, VERDICT_SYSTEM};
use crate::jsonutil::extract_json_object;
use crate::{
    parse_dispatch_plan, parse_verdict, Commander, DispatchContext, DispatchPlan, LlmUsage,
    VerdictContext, VerdictDraft,
};
use async_trait::async_trait;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use wanax_core::config::{commander_cli_args, commander_cli_bin, ResolvedConfig};
use wanax_core::envenv;
use wanax_core::error::{ErrorCode, WanaxError};
use wanax_core::sandbox::{self, SandboxSpec};

/// Outer commander that calls a local coding CLI (Claude Code / Codex) for JSON only.
///
/// The process cwd is an isolated scratch directory, the environment is
/// whitelist-sanitized (no WANAX/GitHub keys, no SSH agent), and — when a
/// backend is available — the process is confined by the same OS sandbox the
/// inner worker runs under. "The outer loop does not write code" is enforced
/// mechanically, not by prompt.
#[derive(Debug)]
pub struct CliCommander {
    bin: PathBuf,
    args: Vec<String>,
    model: String,
    scratch: PathBuf,
    sandbox: SandboxSpec,
}

impl CliCommander {
    pub fn from_config(cfg: &ResolvedConfig) -> Result<Self, WanaxError> {
        let name = commander_cli_bin(&cfg.file.commander);
        if name.is_empty() {
            return Err(WanaxError::new(
                ErrorCode::AdapterMissing,
                "commander cli binary not found",
            ));
        }
        let scratch = isolated_scratch()?;
        write_scratch_gitconfig(&scratch)?;
        let spec = sandbox::resolve(
            &cfg.file.sandbox.mode,
            vec![scratch.clone()],
            &cfg.file.sandbox.extra_write_paths,
            Some(scratch.join("commander.sb")),
        )?;
        Ok(Self {
            bin: resolve_bin(&name)?,
            args: commander_cli_args(&cfg.file.commander),
            model: cfg.file.commander.model.clone(),
            scratch,
            sandbox: spec,
        })
    }

    async fn complete(&self, system: &str, user: &str) -> Result<(String, LlmUsage), WanaxError> {
        let prompt = format!("{system}\n\n{user}");
        let (prog, argv) = sandbox::wrap(&self.sandbox, &self.bin, &self.args)?;
        let mut env = envenv::base_child_env();
        envenv::with_git_config(&mut env, &self.scratch.join("gitconfig"));
        env.insert("WANAX_COMMANDER_MODEL".into(), self.model.clone());
        let mut cmd = Command::new(prog);
        cmd.args(argv)
            .current_dir(&self.scratch)
            .env_clear()
            .envs(&env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.as_std_mut().process_group(0);
        }
        let mut child = cmd.spawn().map_err(|e| {
            WanaxError::new(
                ErrorCode::AdapterMissing,
                format!("commander cli binary not found: {}: {e}", self.bin.display()),
            )
        })?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .map_err(|e| WanaxError::with_detail(ErrorCode::CommanderSchema, e))?;
        }
        let pid = child.id().unwrap_or(0);
        let out = match tokio::time::timeout(
            std::time::Duration::from_secs(120),
            child.wait_with_output(),
        )
        .await
        {
            Ok(v) => v.map_err(|e| WanaxError::with_detail(ErrorCode::CommanderSchema, e))?,
            Err(_) => {
                kill_group(pid);
                return Err(WanaxError::from_code(ErrorCode::WorkerTimeout));
            }
        };
        if !out.status.success() {
            // CLIs often put the actionable error on stdout, not stderr —
            // e.g. claude emits its JSON envelope even on failure
            // ("Not logged in"). Decode it for a clean message; otherwise
            // dump both streams (truncated).
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            unwrap_cli_json_envelope(&stdout)?;
            let stderr_tail = tail_8000_str(&out.stderr);
            let stdout_tail = tail_8000_str(&out.stdout);
            return Err(WanaxError::new(
                ErrorCode::CommanderSchema,
                format!("commander cli failed: {stderr_tail}{stdout_tail}"),
            ));
        }
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        // `claude -p --output-format json` wraps the answer in an envelope.
        let text = unwrap_cli_json_envelope(&text)?;
        let usage = LlmUsage {
            chars_in: prompt.len() as u64,
            chars_out: text.len() as u64,
            raw_json: text.clone(),
            ..LlmUsage::default()
        };
        Ok((text, usage))
    }
}

fn tail_8000_str(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let start = s.len().saturating_sub(8000);
    s[start..].to_string()
}

/// If stdout is a Claude `--output-format json` envelope, return `result`;
/// an `is_error` envelope maps to a commander failure. Raw text passes through.
fn unwrap_cli_json_envelope(stdout: &str) -> Result<String, WanaxError> {
    let trimmed = stdout.trim();
    if !trimmed.starts_with('{') {
        return Ok(stdout.to_string());
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Ok(stdout.to_string());
    };
    let Some(result) = v.get("result") else {
        return Ok(stdout.to_string());
    };
    if v.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false) {
        return Err(WanaxError::new(
            ErrorCode::CommanderSchema,
            format!(
                "commander cli reported error: {}",
                result.as_str().unwrap_or("")
            ),
        ));
    }
    Ok(result.as_str().unwrap_or_default().to_string())
}

fn kill_group(pid: u32) {
    if pid > 0 {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &format!("-{pid}")])
            .status();
    }
}

fn write_scratch_gitconfig(scratch: &std::path::Path) -> Result<(), WanaxError> {
    std::fs::write(
        scratch.join("gitconfig"),
        "[credential]\n\thelper =\n[commit]\n\tgpgsign = false\n[user]\n\tname = wanax\n\temail = wanax@local\n",
    )
    .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))
}

fn isolated_scratch() -> Result<PathBuf, WanaxError> {
    let dir = std::env::temp_dir().join(format!("wanax-commander-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    // Stop git / coding CLIs from walking up into the user's repository.
    std::fs::create_dir_all(dir.join(".git")).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    Ok(dir)
}

fn resolve_bin(name: &str) -> Result<PathBuf, WanaxError> {
    let p = PathBuf::from(name);
    if p.is_file() {
        return Ok(p);
    }
    which::which(name).map_err(|_| {
        WanaxError::new(
            ErrorCode::AdapterMissing,
            format!("commander cli binary not found: {name}"),
        )
    })
}

#[async_trait]
impl Commander for CliCommander {
    async fn dispatch_plan(
        &self,
        ctx: &DispatchContext,
    ) -> Result<(DispatchPlan, LlmUsage), WanaxError> {
        let user = crate::format_dispatch_instruction(ctx);
        let (text, usage) = self.complete(DISPATCH_SYSTEM, &user).await?;
        let json = extract_json_object(&text)
            .ok_or_else(|| WanaxError::from_code(ErrorCode::CommanderSchema))?;
        let (plan, _) = parse_dispatch_plan(json)?;
        Ok((plan, usage))
    }

    async fn verdict(&self, ctx: &VerdictContext) -> Result<(VerdictDraft, LlmUsage), WanaxError> {
        let user = verdict_user(ctx);
        let (text, usage) = self.complete(VERDICT_SYSTEM, &user).await?;
        let json = extract_json_object(&text)
            .ok_or_else(|| WanaxError::from_code(ErrorCode::CommanderSchema))?;
        let (draft, _) = parse_verdict(json)?;
        Ok((draft, usage))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DispatchContext;
    use wanax_core::config::FileConfig;
    use wanax_core::types::{CompletionCriterion, Contract};

    #[tokio::test]
    async fn cli_commander_reads_json_from_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("claude");
        let pwd_file = dir.path().join("pwd");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\npwd > '{}'\ncat >/dev/null\nprintf '%s' '{{\"title\":\"add-fn\",\"instruction\":\"do the work. allowed: src/**. test_command: cargo test. CC-01: pass\"}}'\n",
                pwd_file.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut file = FileConfig::default();
        file.commander.provider = "claude_cli".into();
        file.commander.cli_bin = script.display().to_string();
        let cfg = ResolvedConfig::from_file(file).unwrap();
        let cmd = CliCommander::from_config(&cfg).unwrap();
        let (plan, _) = cmd
            .dispatch_plan(&DispatchContext {
                contract: Contract {
                    id: "wx_01AAAAAAAAAAAAAAAAAAAAAAAA".into(),
                    path: "specs/a.md".into(),
                    content_sha256: "ab".repeat(32),
                    intent: "add".into(),
                    decisions: vec!["d".into()],
                    allowed_globs: vec!["src/**".into()],
                    forbidden_globs: vec![],
                    forbidden_rules: vec![],
                    completion_criteria: vec![CompletionCriterion {
                        id: "CC-01".into(),
                        statement: "pass".into(),
                        bound_test: None,
                        must_have_files: vec![],
                    }],
                    test_command: "cargo test".into(),
                    test_timeout_secs: 30,
                    name: Some("add-fn".into()),
                    agent_spec: None,
                },
                rework_notes: None,
                rsi_notes: None,
            })
            .await
            .unwrap();
        match plan {
            DispatchPlan::Single(d) => assert_eq!(d.title, "add-fn"),
            _ => panic!("expected single unit"),
        }
        let cwd = std::fs::read_to_string(&pwd_file).unwrap();
        assert!(
            cwd.contains("wanax-commander-"),
            "commander cli must not run in the repo: {cwd}"
        );
    }

    #[test]
    fn cli_json_envelope_unwrapped() {
        let out = unwrap_cli_json_envelope(
            "{\"result\":\"{\\\"title\\\":\\\"t\\\"}\",\"is_error\":false}",
        )
        .unwrap();
        assert_eq!(out, "{\"title\":\"t\"}");
        let err = unwrap_cli_json_envelope(
            "{\"result\":\"denied\",\"is_error\":true}",
        );
        assert!(err.is_err());
        // Plain text (codex, or claude without json flag) passes through.
        assert_eq!(
            unwrap_cli_json_envelope("{\"title\":\"t\"}").unwrap(),
            "{\"title\":\"t\"}"
        );
    }

    #[test]
    fn missing_cli_bin_is_adapter_missing() {
        let mut file = FileConfig::default();
        file.commander.provider = "claude_cli".into();
        file.commander.cli_bin = "wanax-missing-commander-cli-9f3a".into();
        let cfg = ResolvedConfig::from_file(file).unwrap();
        let err = CliCommander::from_config(&cfg).unwrap_err();
        assert_eq!(err.code, ErrorCode::AdapterMissing);
    }
}
