use std::path::{Path, PathBuf};
use std::process::Command;
use wanax_core::config::ResolvedConfig;
use wanax_core::error::{ErrorCode, WanaxError};
use wanax_core::types::Contract;

#[derive(Debug, Clone)]
pub struct PluginReport {
    pub ran: bool,
    pub ok: bool,
    pub skipped: bool,
    pub name: String,
    pub excerpt: String,
}

impl PluginReport {
    pub fn skipped(name: &str, why: &str) -> Self {
        Self {
            ran: false,
            ok: true,
            skipped: true,
            name: name.into(),
            excerpt: why.into(),
        }
    }
}

/// Optional agent-spec L1–L3 gate (`lint` + `verify`) plus `lifecycle`.
/// Never a hard crate dependency.
pub fn run_verifier_plugins(
    cfg: &ResolvedConfig,
    contract: &Contract,
    code_root: &Path,
    repo: &Path,
) -> Result<PluginReport, WanaxError> {
    if !cfg
        .file
        .verify
        .plugins
        .iter()
        .any(|p| p == "agent-spec" || p == "agent_spec")
    {
        return Ok(PluginReport::skipped("agent-spec", "not enabled"));
    }
    let bin = if cfg.file.verify.agent_spec_bin.trim().is_empty() {
        "agent-spec".to_string()
    } else {
        cfg.file.verify.agent_spec_bin.clone()
    };
    let resolved = which_bin(&bin);
    if resolved.is_none() {
        if cfg.file.verify.require_plugins {
            return Err(WanaxError::with_detail(
                ErrorCode::Plugin,
                format!("verifier plugin missing: {bin}"),
            ));
        }
        return Ok(PluginReport::skipped("agent-spec", "binary missing"));
    }
    let spec = agent_spec_path(contract, repo);
    let Some(spec) = spec else {
        if cfg.file.verify.require_plugins {
            return Err(WanaxError::with_detail(
                ErrorCode::Plugin,
                "agent_spec path missing on contract",
            ));
        }
        return Ok(PluginReport::skipped("agent-spec", "no spec path"));
    };
    let bin = resolved.unwrap();
    let commands = if cfg.file.verify.agent_spec_commands.is_empty() {
        vec![
            "lint".to_string(),
            "verify".to_string(),
            "lifecycle".to_string(),
        ]
    } else {
        cfg.file.verify.agent_spec_commands.clone()
    };
    let mut excerpt = String::new();
    let mut ok = true;
    for command in &commands {
        let args = command_args(command, &spec, code_root);
        let out = Command::new(&bin)
            .args(&args)
            .current_dir(code_root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|e| WanaxError::with_detail(ErrorCode::Plugin, e))?;
        excerpt.push_str(&format!("# {command}\n"));
        excerpt.push_str(&String::from_utf8_lossy(&out.stdout));
        excerpt.push_str(&String::from_utf8_lossy(&out.stderr));
        excerpt.push('\n');
        if !out.status.success() {
            ok = false;
            if cfg.file.verify.require_plugins {
                if excerpt.chars().count() > 4000 {
                    excerpt = excerpt.chars().take(4000).collect();
                }
                return Err(WanaxError::with_detail(
                    ErrorCode::Plugin,
                    format!("agent-spec {command} failed: {excerpt}"),
                ));
            }
            break;
        }
    }
    if excerpt.chars().count() > 4000 {
        excerpt = excerpt.chars().take(4000).collect();
    }
    Ok(PluginReport {
        ran: true,
        ok,
        skipped: false,
        name: "agent-spec".into(),
        excerpt,
    })
}

fn command_args(command: &str, spec: &Path, code_root: &Path) -> Vec<String> {
    let spec = spec.to_str().unwrap_or(".").to_string();
    let code = code_root.to_str().unwrap_or(".").to_string();
    match command {
        "lint" => vec!["lint".into(), spec],
        "verify" => vec![
            "verify".into(),
            spec,
            "--code".into(),
            code,
            "--format".into(),
            "json".into(),
        ],
        _ => vec![
            command.into(),
            spec,
            "--code".into(),
            code,
            "--format".into(),
            "json".into(),
        ],
    }
}

pub fn agent_spec_path(contract: &Contract, repo: &Path) -> Option<PathBuf> {
    let rel = contract.agent_spec.as_deref()?;
    let p = if Path::new(rel).is_absolute() {
        PathBuf::from(rel)
    } else {
        repo.join(rel)
    };
    p.is_file().then_some(p)
}

fn which_bin(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return p.is_file().then_some(p);
    }
    let path = std::env::var("PATH").ok()?;
    for dir in path.split(':') {
        let cand = Path::new(dir).join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use wanax_core::config::FileConfig;

    #[test]
    fn disabled_plugin_is_skipped() {
        let cfg = wanax_core::ResolvedConfig::from_file(FileConfig::default()).unwrap();
        let contract = Contract {
            id: "wx_01AAAAAAAAAAAAAAAAAAAAAAAA".into(),
            path: "specs/a.md".into(),
            content_sha256: "ab".repeat(32),
            intent: "x".into(),
            decisions: vec!["d".into()],
            allowed_globs: vec!["src/**".into()],
            forbidden_globs: vec![],
            forbidden_rules: vec![],
            completion_criteria: vec![],
            test_command: "cargo test".into(),
            test_timeout_secs: 30,
            name: None,
            agent_spec: None,
        };
        let r = run_verifier_plugins(&cfg, &contract, Path::new("."), Path::new(".")).unwrap();
        assert!(r.skipped);
        assert!(r.ok);
        assert!(!r.ran);
    }

    #[test]
    fn lint_verify_lifecycle_args() {
        let spec = Path::new("/tmp/task.spec");
        let code = Path::new("/tmp/code");
        assert_eq!(
            command_args("lint", spec, code),
            vec!["lint".to_string(), "/tmp/task.spec".into()]
        );
        assert_eq!(
            command_args("verify", spec, code)[0],
            "verify"
        );
        assert_eq!(
            command_args("lifecycle", spec, code)[0],
            "lifecycle"
        );
    }
}
