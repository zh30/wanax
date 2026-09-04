use crate::error::{ErrorCode, WanaxError};
use crate::money::parse_usd_decimal;
use crate::types::{
    WorkerAdapterKind, DEFAULT_MAX_INNER_TURNS, DEFAULT_MAX_USD_MICROS, DEFAULT_WORKER_TIMEOUT_SECS,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileConfig {
    #[serde(default)]
    pub commander: CommanderConfig,
    #[serde(default)]
    pub inner: InnerConfig,
    #[serde(default)]
    pub reviewer: ReviewerConfig,
    #[serde(default)]
    pub worker: WorkerConfig,
    #[serde(default)]
    pub budget: BudgetConfig,
    #[serde(default)]
    pub git: GitConfig,
    #[serde(default)]
    pub github: GitHubConfig,
    #[serde(default)]
    pub test: TestConfig,
    #[serde(default)]
    pub lock: LockConfig,
    #[serde(default)]
    pub verify: VerifyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommanderConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_commander_model")]
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
}

impl Default for CommanderConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            model: default_commander_model(),
            base_url: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InnerConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_inner_model")]
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
}

impl Default for InnerConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            model: default_inner_model(),
            base_url: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReviewerConfig {
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerConfig {
    #[serde(default = "default_adapter")]
    pub adapter: String,
    #[serde(default = "default_octoscode_bin")]
    pub octoscode_bin: String,
    #[serde(default)]
    pub cmd: String,
    #[serde(default)]
    pub cmd_args: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u32,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            adapter: default_adapter(),
            octoscode_bin: default_octoscode_bin(),
            cmd: String::new(),
            cmd_args: Vec::new(),
            timeout_secs: default_timeout(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EstimateRate {
    #[serde(default)]
    pub usd_per_million_chars_in: String,
    #[serde(default)]
    pub usd_per_million_chars_out: String,
}

impl Default for EstimateRate {
    fn default() -> Self {
        Self {
            usd_per_million_chars_in: "10.00".into(),
            usd_per_million_chars_out: "50.00".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EstimateRates {
    #[serde(default)]
    pub commander: EstimateRate,
    #[serde(default = "default_inner_rate")]
    pub inner: EstimateRate,
}

fn default_inner_rate() -> EstimateRate {
    EstimateRate {
        usd_per_million_chars_in: "0.30".into(),
        usd_per_million_chars_out: "1.20".into(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    #[serde(default = "default_max_usd")]
    pub max_usd: String,
    #[serde(default = "default_max_turns")]
    pub max_inner_turns: u32,
    #[serde(default)]
    pub estimate_rates: EstimateRates,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            max_usd: default_max_usd(),
            max_inner_turns: default_max_turns(),
            estimate_rates: EstimateRates {
                commander: EstimateRate::default(),
                inner: default_inner_rate(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitConfig {
    #[serde(default = "default_protected")]
    pub protected_refs: Vec<String>,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            protected_refs: default_protected(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitHubConfig {
    #[serde(default)]
    pub create_pr: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestConfig {
    #[serde(default = "default_test_cmd")]
    pub default_command: String,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            default_command: default_test_cmd(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockConfig {
    #[serde(default = "default_true")]
    pub repo_exclusive: bool,
}

impl Default for LockConfig {
    fn default() -> Self {
        Self {
            repo_exclusive: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyConfig {
    #[serde(default)]
    pub plugins: Vec<String>,
    #[serde(default = "default_agent_spec_bin")]
    pub agent_spec_bin: String,
    #[serde(default)]
    pub require_plugins: bool,
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self {
            plugins: Vec::new(),
            agent_spec_bin: default_agent_spec_bin(),
            require_plugins: false,
        }
    }
}

fn default_provider() -> String {
    "openai_compat".into()
}
fn default_commander_model() -> String {
    "commander".into()
}
fn default_inner_model() -> String {
    "inner".into()
}
fn default_adapter() -> String {
    "octoscode".into()
}
fn default_octoscode_bin() -> String {
    "octoscode".into()
}
fn default_timeout() -> u32 {
    DEFAULT_WORKER_TIMEOUT_SECS
}
fn default_max_usd() -> String {
    "5.00".into()
}
fn default_max_turns() -> u32 {
    DEFAULT_MAX_INNER_TURNS
}
fn default_protected() -> Vec<String> {
    vec!["main".into(), "master".into()]
}
fn default_test_cmd() -> String {
    "cargo test".into()
}
fn default_true() -> bool {
    true
}
fn default_agent_spec_bin() -> String {
    "agent-spec".into()
}

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub file: FileConfig,
    pub max_usd_micros: i64,
    pub adapter: WorkerAdapterKind,
    pub commander_in_micros: i64,
    pub commander_out_micros: i64,
    pub inner_in_micros: i64,
    pub inner_out_micros: i64,
}

impl ResolvedConfig {
    pub fn from_file(file: FileConfig) -> Result<Self, WanaxError> {
        let max_usd_micros =
            parse_usd_decimal(&file.budget.max_usd).unwrap_or(DEFAULT_MAX_USD_MICROS);
        if !(0..=100_000_000).contains(&max_usd_micros) {
            return Err(WanaxError::new(
                ErrorCode::ContractInvalid,
                "invalid budget.max_usd",
            ));
        }
        if !(1..=500).contains(&file.budget.max_inner_turns) {
            return Err(WanaxError::new(
                ErrorCode::ContractInvalid,
                "invalid budget.max_inner_turns",
            ));
        }
        if !(30..=14400).contains(&file.worker.timeout_secs) {
            return Err(WanaxError::new(
                ErrorCode::ContractInvalid,
                "invalid worker.timeout_secs",
            ));
        }
        let adapter = WorkerAdapterKind::parse(&file.worker.adapter).ok_or_else(|| {
            WanaxError::new(
                ErrorCode::AdapterMissing,
                format!("adapter binary not found: {}", file.worker.adapter),
            )
        })?;
        let commander_in_micros = parse_usd_decimal(
            &file
                .budget
                .estimate_rates
                .commander
                .usd_per_million_chars_in,
        )
        .unwrap_or(10_000_000);
        let commander_out_micros = parse_usd_decimal(
            &file
                .budget
                .estimate_rates
                .commander
                .usd_per_million_chars_out,
        )
        .unwrap_or(50_000_000);
        let inner_in_micros =
            parse_usd_decimal(&file.budget.estimate_rates.inner.usd_per_million_chars_in)
                .unwrap_or(300_000);
        let inner_out_micros =
            parse_usd_decimal(&file.budget.estimate_rates.inner.usd_per_million_chars_out)
                .unwrap_or(1_200_000);
        Ok(Self {
            file,
            max_usd_micros,
            adapter,
            commander_in_micros,
            commander_out_micros,
            inner_in_micros,
            inner_out_micros,
        })
    }
}

pub fn default_config_toml() -> String {
    r#"[commander]
provider = "openai_compat"
model = "commander"

[inner]
provider = "openai_compat"
model = "inner"

[reviewer]
# empty model degrades self-review (Phase 2)

[worker]
adapter = "octoscode"
octoscode_bin = "octoscode"
timeout_secs = 1800

[budget]
max_usd = "5.00"
max_inner_turns = 40

[budget.estimate_rates.commander]
usd_per_million_chars_in = "10.00"
usd_per_million_chars_out = "50.00"

[budget.estimate_rates.inner]
usd_per_million_chars_in = "0.30"
usd_per_million_chars_out = "1.20"

[git]
protected_refs = ["main", "master"]

[test]
default_command = "cargo test"

[lock]
repo_exclusive = true

[verify]
plugins = []
agent_spec_bin = "agent-spec"
require_plugins = false
"#
    .to_string()
}

pub fn load_merged_config(
    repo_root: &Path,
    global_dir: &Path,
) -> Result<ResolvedConfig, WanaxError> {
    let mut merged = FileConfig::default();
    let global = global_dir.join("config.toml");
    if global.is_file() {
        merged = load_file(&global)?;
    }
    let repo = repo_root.join(".wanax").join("config.toml");
    if repo.is_file() {
        let repo_cfg = load_file(&repo)?;
        merged = overlay(merged, repo_cfg);
    }
    // Phase 1: lock must stay exclusive.
    merged.lock.repo_exclusive = true;
    ResolvedConfig::from_file(merged)
}

fn load_file(path: &Path) -> Result<FileConfig, WanaxError> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        WanaxError::with_detail(ErrorCode::Db, format!("read {}: {e}", path.display()))
    })?;
    toml::from_str(&text).map_err(|e| {
        WanaxError::with_detail(ErrorCode::Db, format!("parse {}: {e}", path.display()))
    })
}

fn is_init_placeholder_commander(c: &CommanderConfig) -> bool {
    c.model == default_commander_model() && c.provider == default_provider() && c.base_url.is_none()
}

fn is_init_placeholder_inner(c: &InnerConfig) -> bool {
    c.model == default_inner_model() && c.provider == default_provider() && c.base_url.is_none()
}

fn overlay(mut base: FileConfig, over: FileConfig) -> FileConfig {
    if !is_init_placeholder_commander(&over.commander) {
        base.commander = over.commander;
    }
    if !is_init_placeholder_inner(&over.inner) {
        base.inner = over.inner;
    }
    base.reviewer = over.reviewer;
    base.worker = over.worker;
    base.budget = over.budget;
    base.git = over.git;
    base.github = over.github;
    base.test = over.test;
    base.lock = over.lock;
    if over.verify.plugins.is_empty()
        && !over.verify.require_plugins
        && over.verify.agent_spec_bin == default_agent_spec_bin()
    {
        // keep global verify when repo still has the init placeholder
    } else {
        base.verify = over.verify;
    }
    base
}

pub fn global_data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("WANAX_DATA_DIR") {
        return PathBuf::from(p);
    }
    home_dir().join(".wanax")
}

pub fn home_dir() -> PathBuf {
    if let Some(h) = std::env::var_os("HOME") {
        return PathBuf::from(h);
    }
    PathBuf::from(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_toml_parses() {
        let f: FileConfig = toml::from_str(&default_config_toml()).unwrap();
        let r = ResolvedConfig::from_file(f).unwrap();
        assert_eq!(r.max_usd_micros, 5_000_000);
        assert_eq!(r.file.budget.max_inner_turns, 40);
        assert!(r.file.lock.repo_exclusive);
    }

    #[test]
    fn overlay_keeps_global_models_when_repo_is_init_placeholder() {
        let global = FileConfig {
            commander: CommanderConfig {
                provider: default_provider(),
                model: "anthropic/claude-opus-5".into(),
                base_url: Some("https://openrouter.ai/api/v1".into()),
            },
            inner: InnerConfig {
                provider: default_provider(),
                model: "z-ai/glm-5.3-flash".into(),
                base_url: Some("https://openrouter.ai/api/v1".into()),
            },
            ..FileConfig::default()
        };
        let repo: FileConfig = toml::from_str(&default_config_toml()).unwrap();
        let merged = overlay(global, repo);
        assert_eq!(merged.commander.model, "anthropic/claude-opus-5");
        assert_eq!(
            merged.commander.base_url.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(merged.inner.model, "z-ai/glm-5.3-flash");
        assert_eq!(
            merged.inner.base_url.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
    }

    #[test]
    fn overlay_uses_repo_when_models_are_explicit() {
        let global = FileConfig {
            commander: CommanderConfig {
                provider: default_provider(),
                model: "global-commander".into(),
                base_url: Some("https://example.test/v1".into()),
            },
            ..FileConfig::default()
        };
        let repo = FileConfig {
            commander: CommanderConfig {
                provider: default_provider(),
                model: "repo-commander".into(),
                base_url: Some("https://repo.test/v1".into()),
            },
            inner: InnerConfig {
                provider: default_provider(),
                model: "repo-inner".into(),
                base_url: None,
            },
            ..FileConfig::default()
        };
        let merged = overlay(global, repo);
        assert_eq!(merged.commander.model, "repo-commander");
        assert_eq!(
            merged.commander.base_url.as_deref(),
            Some("https://repo.test/v1")
        );
        assert_eq!(merged.inner.model, "repo-inner");
    }
}
