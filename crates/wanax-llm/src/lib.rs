mod goal;
mod http;
mod jsonutil;
mod provider;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use wanax_core::config::ResolvedConfig;
use wanax_core::error::{ErrorCode, WanaxError};
use wanax_core::types::{Contract, Receipt, VerdictDecision, WorkUnit};

pub use goal::{
    mechanical_self_review, pick_review_client, run_self_review, self_review_degraded,
    GoalSelfReview,
};
pub use http::HttpCommander;
pub use provider::{
    parse_anthropic_body, parse_openai_body, Completion, CompletionClient, FixtureClient,
    LiveClient, ProviderKind,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkUnitDraft {
    pub title: String,
    pub instruction: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerUnitDraft {
    pub title: String,
    pub instruction: String,
    pub allowed_globs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagUnitDraft {
    pub id: String,
    pub title: String,
    pub instruction: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub allowed_globs: Option<Vec<String>>,
    #[serde(default)]
    pub test_command: Option<String>,
}

#[derive(Debug, Clone)]
pub enum DispatchPlan {
    Single(WorkUnitDraft),
    Peers(Vec<PeerUnitDraft>),
    Dag(Vec<DagUnitDraft>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerdictDraft {
    pub decision: VerdictDecision,
    pub reason: String,
    #[serde(default)]
    pub files_reviewed: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DispatchContext {
    pub contract: Contract,
    pub rework_notes: Option<String>,
}

#[derive(Debug, Clone)]
pub struct VerdictContext {
    pub contract: Contract,
    pub receipt: Receipt,
    pub diffstat: String,
    pub changed_files: Vec<String>,
    pub outer_test_exit_code: i32,
    pub outer_test_excerpt: String,
    pub boundary_ok: bool,
    pub rework_count: u32,
}

#[derive(Debug, Clone, Default)]
pub struct LlmUsage {
    pub chars_in: u64,
    pub chars_out: u64,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub raw_json: String,
}

impl LlmUsage {
    /// Returns (units_in, units_out, cost_estimated).
    /// Provider tokens win when both sides are present; otherwise chars × config rates.
    pub fn charge_units(&self) -> (u64, u64, bool) {
        match (self.prompt_tokens, self.completion_tokens) {
            (Some(units_in), Some(units_out)) => (units_in, units_out, false),
            _ => (self.chars_in, self.chars_out, true),
        }
    }
}

pub fn pick_commander(cfg: &ResolvedConfig) -> Result<Box<dyn Commander>, WanaxError> {
    let model = cfg.file.commander.model.clone();
    if let Some(scripted) = ScriptedCommander::from_env() {
        return Ok(Box::new(scripted));
    }
    if let Ok(dir) = std::env::var("WANAX_LLM_FIXTURE_DIR") {
        let client = FixtureClient::load_dir(std::path::Path::new(&dir))?;
        return Ok(Box::new(HttpCommander::new(Arc::new(client), model)));
    }
    if let Ok(key) = std::env::var("WANAX_COMMANDER_API_KEY") {
        if !key.is_empty() {
            let kind = ProviderKind::parse(&cfg.file.commander.provider)?;
            let client = LiveClient::new(kind, key, cfg.file.commander.base_url.clone())?;
            return Ok(Box::new(HttpCommander::new(Arc::new(client), model)));
        }
    }
    Ok(Box::new(MechanicalCommander::new(model)))
}

#[async_trait]
pub trait Commander: Send + Sync {
    async fn dispatch_plan(
        &self,
        ctx: &DispatchContext,
    ) -> Result<(DispatchPlan, LlmUsage), WanaxError>;
    async fn verdict(&self, ctx: &VerdictContext) -> Result<(VerdictDraft, LlmUsage), WanaxError>;
}

/// Deterministic commander used when no fixture or API key is configured.
pub struct MechanicalCommander {
    pub model: String,
}

impl MechanicalCommander {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
        }
    }
}

pub fn format_dispatch_instruction(ctx: &DispatchContext) -> String {
    let c = &ctx.contract;
    let mut s = String::new();
    s.push_str("# Work unit\n\n");
    s.push_str("## Intent\n\n");
    s.push_str(&c.intent);
    s.push_str("\n\n## Decisions\n\n");
    for d in &c.decisions {
        s.push_str(&format!("- {d}\n"));
    }
    s.push_str("\n## Boundaries\n\nAllowed: ");
    s.push_str(&c.allowed_globs.join(", "));
    s.push_str("\nForbidden: ");
    s.push_str(&c.forbidden_globs.join(", "));
    s.push_str("\n\n## Test command\n\n");
    s.push_str(&c.test_command);
    s.push_str("\n\n## Completion criteria\n\n");
    for cc in &c.completion_criteria {
        s.push_str(&format!("- {}: {}\n", cc.id, cc.statement));
    }
    if let Some(notes) = &ctx.rework_notes {
        s.push_str("\n## Rework notes\n\n");
        s.push_str(notes);
        s.push('\n');
    }
    s
}

#[async_trait]
impl Commander for MechanicalCommander {
    async fn dispatch_plan(
        &self,
        ctx: &DispatchContext,
    ) -> Result<(DispatchPlan, LlmUsage), WanaxError> {
        let title = ctx
            .contract
            .name
            .clone()
            .unwrap_or_else(|| truncate(&ctx.contract.intent, 120));
        let instruction = format_dispatch_instruction(ctx);
        let draft = WorkUnitDraft { title, instruction };
        let raw = serde_json::to_string(&draft).unwrap_or_else(|_| "{}".into());
        let usage = LlmUsage {
            chars_in: ctx.contract.intent.len() as u64,
            chars_out: raw.len() as u64,
            prompt_tokens: None,
            completion_tokens: None,
            raw_json: raw,
        };
        Ok((DispatchPlan::Single(draft), usage))
    }

    async fn verdict(&self, ctx: &VerdictContext) -> Result<(VerdictDraft, LlmUsage), WanaxError> {
        let (decision, reason) = if ctx.outer_test_exit_code != 0 {
            (
                VerdictDecision::Rework,
                format!(
                    "outer test exit {}: {}",
                    ctx.outer_test_exit_code,
                    truncate(&ctx.outer_test_excerpt, 400)
                ),
            )
        } else if !ctx.boundary_ok {
            (VerdictDecision::Reject, "boundary check failed".to_string())
        } else if ctx.receipt.changed_files.is_empty() {
            (VerdictDecision::Reject, "no files changed".to_string())
        } else {
            (
                VerdictDecision::Accept,
                "mechanical gates passed".to_string(),
            )
        };
        let draft = VerdictDraft {
            decision,
            reason,
            files_reviewed: ctx.changed_files.clone(),
        };
        let raw = serde_json::to_string(&draft).unwrap_or_else(|_| "{}".into());
        let usage = LlmUsage {
            chars_in: ctx.diffstat.len() as u64 + ctx.outer_test_excerpt.len() as u64,
            chars_out: raw.len() as u64,
            prompt_tokens: None,
            completion_tokens: None,
            raw_json: raw,
        };
        Ok((draft, usage))
    }
}

/// Test seam: JSON file or env-driven scripted responses. Not a product command.
pub struct ScriptedCommander {
    pub dispatch_raw: Vec<String>,
    pub verdict_raw: Vec<String>,
}

impl ScriptedCommander {
    pub fn from_env() -> Option<Self> {
        let path = std::env::var("WANAX_COMMANDER_SCRIPT").ok()?;
        let text = std::fs::read_to_string(path).ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        let dispatch = match &v["dispatch"] {
            serde_json::Value::Array(a) => a.iter().map(|x| x.to_string()).collect(),
            other if !other.is_null() => vec![other.to_string()],
            _ => vec!["{}".into()],
        };
        let verdict = match &v["verdicts"] {
            serde_json::Value::Array(a) => a.iter().map(|x| x.to_string()).collect(),
            other if !other.is_null() => vec![other.to_string()],
            _ => vec!["{}".into()],
        };
        Some(Self {
            dispatch_raw: dispatch,
            verdict_raw: verdict,
        })
    }
}

#[async_trait]
impl Commander for ScriptedCommander {
    async fn dispatch_plan(
        &self,
        _ctx: &DispatchContext,
    ) -> Result<(DispatchPlan, LlmUsage), WanaxError> {
        let raw = self
            .dispatch_raw
            .first()
            .cloned()
            .unwrap_or_else(|| "{}".into());
        parse_dispatch_plan(&raw)
    }

    async fn verdict(&self, ctx: &VerdictContext) -> Result<(VerdictDraft, LlmUsage), WanaxError> {
        let idx = ctx.rework_count as usize;
        let raw = self
            .verdict_raw
            .get(idx)
            .cloned()
            .or_else(|| self.verdict_raw.last().cloned())
            .unwrap_or_else(|| "{}".into());
        parse_verdict(&raw)
    }
}

pub fn parse_dispatch(raw: &str) -> Result<(WorkUnitDraft, LlmUsage), WanaxError> {
    match parse_dispatch_plan(raw)? {
        (DispatchPlan::Single(draft), usage) => Ok((draft, usage)),
        (DispatchPlan::Peers(_) | DispatchPlan::Dag(_), _) => {
            Err(WanaxError::from_code(ErrorCode::CommanderSchema))
        }
    }
}

pub fn parse_dispatch_plan(raw: &str) -> Result<(DispatchPlan, LlmUsage), WanaxError> {
    let v: serde_json::Value = serde_json::from_str(raw)
        .map_err(|_| WanaxError::from_code(ErrorCode::CommanderSchema))?;
    let usage = LlmUsage {
        chars_in: 0,
        chars_out: raw.len() as u64,
        prompt_tokens: None,
        completion_tokens: None,
        raw_json: raw.to_string(),
    };
    if let Some(units) = v.get("units").and_then(|p| p.as_array()) {
        if units.is_empty() || units.len() > 16 {
            return Err(WanaxError::from_code(ErrorCode::CommanderSchema));
        }
        let mut out = Vec::with_capacity(units.len());
        for u in units {
            let draft: DagUnitDraft = serde_json::from_value(u.clone())
                .map_err(|_| WanaxError::from_code(ErrorCode::CommanderSchema))?;
            if draft.id.is_empty()
                || draft.id.len() > 64
                || draft.title.is_empty()
                || draft.title.len() > 120
                || draft.instruction.is_empty()
                || draft.instruction.len() > 8000
            {
                return Err(WanaxError::from_code(ErrorCode::CommanderSchema));
            }
            out.push(draft);
        }
        let nodes: Vec<(String, Vec<String>)> = out
            .iter()
            .map(|d| (d.id.clone(), d.depends_on.clone()))
            .collect();
        wanax_core::dag::topo_sort(&nodes)?;
        return Ok((DispatchPlan::Dag(out), usage));
    }
    if let Some(peers) = v.get("peers").and_then(|p| p.as_array()) {
        if peers.is_empty() || peers.len() > 8 {
            return Err(WanaxError::from_code(ErrorCode::CommanderSchema));
        }
        let mut out = Vec::with_capacity(peers.len());
        for p in peers {
            let draft: PeerUnitDraft = serde_json::from_value(p.clone())
                .map_err(|_| WanaxError::from_code(ErrorCode::CommanderSchema))?;
            if draft.title.is_empty()
                || draft.title.len() > 120
                || draft.instruction.is_empty()
                || draft.instruction.len() > 8000
                || draft.allowed_globs.is_empty()
            {
                return Err(WanaxError::from_code(ErrorCode::CommanderSchema));
            }
            out.push(draft);
        }
        return Ok((DispatchPlan::Peers(out), usage));
    }
    let draft: WorkUnitDraft =
        serde_json::from_str(raw).map_err(|_| WanaxError::from_code(ErrorCode::CommanderSchema))?;
    if draft.title.is_empty() || draft.title.len() > 120 || draft.instruction.is_empty() {
        return Err(WanaxError::from_code(ErrorCode::CommanderSchema));
    }
    if draft.instruction.len() > 8000 {
        return Err(WanaxError::from_code(ErrorCode::CommanderSchema));
    }
    Ok((DispatchPlan::Single(draft), usage))
}

pub fn parse_verdict(raw: &str) -> Result<(VerdictDraft, LlmUsage), WanaxError> {
    let draft: VerdictDraft =
        serde_json::from_str(raw).map_err(|_| WanaxError::from_code(ErrorCode::CommanderSchema))?;
    if draft.reason.is_empty() || draft.reason.len() > 2000 {
        return Err(WanaxError::from_code(ErrorCode::CommanderSchema));
    }
    Ok((
        draft,
        LlmUsage {
            chars_in: 0,
            chars_out: raw.len() as u64,
            prompt_tokens: None,
            completion_tokens: None,
            raw_json: raw.to_string(),
        },
    ))
}

pub async fn dispatch_with_retry(
    commander: &dyn Commander,
    ctx: &DispatchContext,
) -> Result<(DispatchPlan, LlmUsage), WanaxError> {
    let mut last = WanaxError::from_code(ErrorCode::CommanderSchema);
    for _ in 0..3 {
        match commander.dispatch_plan(ctx).await {
            Ok(v) => {
                match &v.0 {
                    DispatchPlan::Single(d) if d.title.is_empty() || d.instruction.is_empty() => {
                        last = WanaxError::from_code(ErrorCode::CommanderSchema);
                        continue;
                    }
                    DispatchPlan::Peers(peers) if peers.is_empty() => {
                        last = WanaxError::from_code(ErrorCode::CommanderSchema);
                        continue;
                    }
                    DispatchPlan::Dag(units) if units.is_empty() => {
                        last = WanaxError::from_code(ErrorCode::CommanderSchema);
                        continue;
                    }
                    _ => return Ok(v),
                }
            }
            Err(e) if e.code == ErrorCode::CommanderSchema => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// Unused in Phase 1 except as a type placeholder so the crate boundary exists.
#[allow(dead_code)]
pub fn work_unit_from_draft(run_id: &str, seq: u32, draft: WorkUnitDraft) -> WorkUnit {
    WorkUnit {
        id: wanax_core::new_id(),
        run_id: run_id.to_string(),
        seq,
        title: draft.title,
        instruction: draft.instruction,
        state: wanax_core::WorkUnitState::Queued,
        assignee_role: wanax_core::AssigneeRole::Goal,
        parent_id: None,
        allowed_globs: None,
        depends_on: Vec::new(),
        test_command: None,
        local_key: None,
        rework_count: 0,
        inner_commit_sha: None,
        receipt_id: None,
        verdict_id: None,
    }
}
