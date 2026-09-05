use crate::http::{DISPATCH_SYSTEM, VERDICT_SYSTEM};
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
use wanax_core::error::{ErrorCode, WanaxError};

/// Outer commander that calls a local coding CLI (Claude Code / Codex) for JSON only.
///
/// The process cwd is an isolated scratch directory so a skip-permissions CLI
/// cannot write the target repo as a side effect.
#[derive(Debug)]
pub struct CliCommander {
    bin: PathBuf,
    args: Vec<String>,
    model: String,
    scratch: PathBuf,
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
        Ok(Self {
            bin: resolve_bin(&name)?,
            args: commander_cli_args(&cfg.file.commander),
            model: cfg.file.commander.model.clone(),
            scratch: isolated_scratch()?,
        })
    }

    async fn complete(&self, system: &str, user: &str) -> Result<(String, LlmUsage), WanaxError> {
        let prompt = format!("{system}\n\n{user}");
        let mut cmd = Command::new(&self.bin);
        cmd.args(&self.args)
            .current_dir(&self.scratch)
            .env("WANAX_COMMANDER_MODEL", &self.model)
            .env_remove("GIT_ASKPASS")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
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
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| WanaxError::from_code(ErrorCode::WorkerTimeout))?
        .map_err(|e| WanaxError::with_detail(ErrorCode::CommanderSchema, e))?;
        if !out.status.success() {
            return Err(WanaxError::new(
                ErrorCode::CommanderSchema,
                format!(
                    "commander cli failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                ),
            ));
        }
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let usage = LlmUsage {
            chars_in: prompt.len() as u64,
            chars_out: text.len() as u64,
            prompt_tokens: None,
            completion_tokens: None,
            raw_json: text.clone(),
        };
        Ok((text, usage))
    }
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
        let user = format!(
            "outer_test_exit_code={}\nboundary_ok={}\nclaimed_pass={}\nrework_count={}\n\
changed_files:\n{}\n\ndiffstat:\n{}\n\nexcerpt:\n{}\n",
            ctx.outer_test_exit_code,
            ctx.boundary_ok,
            ctx.receipt.claimed_pass,
            ctx.rework_count,
            ctx.changed_files.join("\n"),
            ctx.diffstat,
            ctx.outer_test_excerpt
        );
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
    fn missing_cli_bin_is_adapter_missing() {
        let mut file = FileConfig::default();
        file.commander.provider = "claude_cli".into();
        file.commander.cli_bin = "wanax-missing-commander-cli-9f3a".into();
        let cfg = ResolvedConfig::from_file(file).unwrap();
        let err = CliCommander::from_config(&cfg).unwrap_err();
        assert_eq!(err.code, ErrorCode::AdapterMissing);
    }
}
