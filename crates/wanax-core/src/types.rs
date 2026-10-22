use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Draft,
    ContractReady,
    Dispatched,
    InnerWorking,
    ReceiptReady,
    OuterReviewing,
    Accepted,
    Rejected,
    Rework,
    Escalate,
    Canceling,
    Cancelled,
    BudgetExhausted,
    Failed,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::ContractReady => "contract_ready",
            Self::Dispatched => "dispatched",
            Self::InnerWorking => "inner_working",
            Self::ReceiptReady => "receipt_ready",
            Self::OuterReviewing => "outer_reviewing",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Rework => "rework",
            Self::Escalate => "escalate",
            Self::Canceling => "canceling",
            Self::Cancelled => "cancelled",
            Self::BudgetExhausted => "budget_exhausted",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "draft" => Self::Draft,
            "contract_ready" => Self::ContractReady,
            "dispatched" => Self::Dispatched,
            "inner_working" => Self::InnerWorking,
            "receipt_ready" => Self::ReceiptReady,
            "outer_reviewing" => Self::OuterReviewing,
            "accepted" => Self::Accepted,
            "rejected" => Self::Rejected,
            "rework" => Self::Rework,
            "escalate" => Self::Escalate,
            "canceling" => Self::Canceling,
            "cancelled" => Self::Cancelled,
            "budget_exhausted" => Self::BudgetExhausted,
            "failed" => Self::Failed,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Accepted
                | Self::Rejected
                | Self::Cancelled
                | Self::BudgetExhausted
                | Self::Failed
                | Self::Escalate
        )
    }
}

impl std::fmt::Display for RunState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkUnitState {
    Queued,
    Assigned,
    Implementing,
    SelfVerifying,
    ReceiptReady,
    OuterTesting,
    Accepted,
    Rejected,
    Blocked,
}

impl WorkUnitState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Assigned => "assigned",
            Self::Implementing => "implementing",
            Self::SelfVerifying => "self_verifying",
            Self::ReceiptReady => "receipt_ready",
            Self::OuterTesting => "outer_testing",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Blocked => "blocked",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => Self::Queued,
            "assigned" => Self::Assigned,
            "implementing" => Self::Implementing,
            "self_verifying" => Self::SelfVerifying,
            "receipt_ready" => Self::ReceiptReady,
            "outer_testing" => Self::OuterTesting,
            "accepted" => Self::Accepted,
            "rejected" => Self::Rejected,
            "blocked" => Self::Blocked,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssigneeRole {
    Master,
    Goal,
    Peer,
}

impl AssigneeRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Master => "master",
            Self::Goal => "goal",
            Self::Peer => "peer",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "master" => Self::Master,
            "goal" => Self::Goal,
            "peer" => Self::Peer,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerAdapterKind {
    Octoscode,
    Fake,
    Cmd,
    Claude,
    Codex,
    Acp,
}

impl WorkerAdapterKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Octoscode => "octoscode",
            Self::Fake => "fake",
            Self::Cmd => "cmd",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Acp => "acp",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "octoscode" => Self::Octoscode,
            "fake" => Self::Fake,
            "cmd" => Self::Cmd,
            "claude" => Self::Claude,
            "codex" => Self::Codex,
            "acp" => Self::Acp,
            _ => return None,
        })
    }

    pub fn is_supported(self) -> bool {
        matches!(
            self,
            Self::Octoscode | Self::Fake | Self::Cmd | Self::Claude | Self::Codex | Self::Acp
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictDecision {
    Accept,
    Reject,
    Rework,
    Escalate,
}

impl VerdictDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Reject => "reject",
            Self::Rework => "rework",
            Self::Escalate => "escalate",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "accept" => Self::Accept,
            "reject" => Self::Reject,
            "rework" => Self::Rework,
            "escalate" => Self::Escalate,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionCriterion {
    pub id: String,
    pub statement: String,
    #[serde(default)]
    pub bound_test: Option<String>,
    #[serde(default)]
    pub must_have_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contract {
    pub id: String,
    pub path: String,
    pub content_sha256: String,
    pub intent: String,
    pub decisions: Vec<String>,
    pub allowed_globs: Vec<String>,
    pub forbidden_globs: Vec<String>,
    #[serde(default)]
    pub forbidden_rules: Vec<String>,
    pub completion_criteria: Vec<CompletionCriterion>,
    pub test_command: String,
    pub test_timeout_secs: u32,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub agent_spec: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactoryRun {
    pub id: String,
    pub repo_root: String,
    pub contract_id: String,
    pub contract_sha256: String,
    pub state: RunState,
    pub base_sha: String,
    pub inner_branch: String,
    pub outer_branch: String,
    pub commander_model: String,
    pub inner_model: String,
    pub reviewer_model: Option<String>,
    pub max_usd_micros: i64,
    pub max_inner_turns: u32,
    pub spent_usd_micros: i64,
    pub spent_inner_turns: u32,
    pub worker_adapter: WorkerAdapterKind,
    pub created_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
    pub last_error: Option<String>,
    pub worker_pid: Option<i64>,
    pub start_pid: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkUnit {
    pub id: String,
    pub run_id: String,
    pub seq: u32,
    pub title: String,
    pub instruction: String,
    pub state: WorkUnitState,
    pub assignee_role: AssigneeRole,
    pub parent_id: Option<String>,
    pub allowed_globs: Option<Vec<String>>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub test_command: Option<String>,
    #[serde(default)]
    pub local_key: Option<String>,
    pub rework_count: u32,
    pub inner_commit_sha: Option<String>,
    pub receipt_id: Option<String>,
    pub verdict_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub id: String,
    pub work_unit_id: String,
    pub changed_files: Vec<String>,
    pub diffstat: String,
    pub commit_sha: String,
    pub test_command: String,
    pub test_exit_code: i32,
    pub test_excerpt: String,
    pub claimed_pass: bool,
    pub duration_ms: u64,
    pub adapter: String,
    pub raw_artifact_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub id: String,
    pub work_unit_id: String,
    pub decision: VerdictDecision,
    pub reason: String,
    pub outer_test_exit_code: i32,
    pub outer_test_excerpt: String,
    pub boundary_ok: bool,
    pub files_reviewed: Vec<String>,
    pub commander_model: String,
    pub created_at: String,
}

pub const MAX_REWORK: u32 = 3;
pub const MAX_GOAL_ITERS: u32 = 8;
pub const CANCEL_TIMEOUT_SECS: u64 = 20;
pub const DEFAULT_MAX_USD_MICROS: i64 = 5_000_000;
pub const DEFAULT_MAX_INNER_TURNS: u32 = 40;
pub const DEFAULT_TEST_TIMEOUT_SECS: u32 = 300;
pub const DEFAULT_WORKER_TIMEOUT_SECS: u32 = 1800;

pub fn inner_branch_name(run_id: &str) -> String {
    format!("wanax/{run_id}/inner")
}

pub fn outer_branch_name(run_id: &str) -> String {
    format!("wanax/{run_id}/outer")
}

pub fn peer_branch_name(run_id: &str, seq: u32) -> String {
    format!("wanax/{run_id}/peer-{seq}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_run_states() -> [RunState; 14] {
        [
            RunState::Draft,
            RunState::ContractReady,
            RunState::Dispatched,
            RunState::InnerWorking,
            RunState::ReceiptReady,
            RunState::OuterReviewing,
            RunState::Accepted,
            RunState::Rejected,
            RunState::Rework,
            RunState::Escalate,
            RunState::Canceling,
            RunState::Cancelled,
            RunState::BudgetExhausted,
            RunState::Failed,
        ]
    }

    #[test]
    fn rework_is_not_terminal() {
        assert!(!RunState::Rework.is_terminal());
        assert!(RunState::Escalate.is_terminal());
        assert!(RunState::Accepted.is_terminal());
    }

    #[test]
    fn run_state_parse_roundtrip() {
        for state in all_run_states() {
            assert_eq!(RunState::parse(state.as_str()), Some(state));
        }
        assert_eq!(RunState::parse(""), None);
        assert_eq!(RunState::parse("nope"), None);
        // parse matches the snake_case encoding exactly; case variants miss.
        assert_eq!(RunState::parse("DRAFT"), None);
        assert_eq!(RunState::parse("Draft"), None);
    }

    #[test]
    fn run_state_is_terminal_exhaustive() {
        // Terminal set from `RunState::is_terminal`: accepted, rejected,
        // cancelled, budget_exhausted, failed, escalate. `rework` and the
        // in-flight `canceling` state are not terminal.
        let cases = [
            (RunState::Draft, false),
            (RunState::ContractReady, false),
            (RunState::Dispatched, false),
            (RunState::InnerWorking, false),
            (RunState::ReceiptReady, false),
            (RunState::OuterReviewing, false),
            (RunState::Accepted, true),
            (RunState::Rejected, true),
            (RunState::Rework, false),
            (RunState::Escalate, true),
            (RunState::Canceling, false),
            (RunState::Cancelled, true),
            (RunState::BudgetExhausted, true),
            (RunState::Failed, true),
        ];
        assert_eq!(cases.len(), all_run_states().len());
        for (state, terminal) in cases {
            assert_eq!(
                state.is_terminal(),
                terminal,
                "{} terminal mismatch",
                state.as_str()
            );
        }
    }

    #[test]
    fn work_unit_state_parse_roundtrip() {
        let all = [
            WorkUnitState::Queued,
            WorkUnitState::Assigned,
            WorkUnitState::Implementing,
            WorkUnitState::SelfVerifying,
            WorkUnitState::ReceiptReady,
            WorkUnitState::OuterTesting,
            WorkUnitState::Accepted,
            WorkUnitState::Rejected,
            WorkUnitState::Blocked,
        ];
        for state in all {
            assert_eq!(WorkUnitState::parse(state.as_str()), Some(state));
        }
        assert_eq!(WorkUnitState::parse("nope"), None);
    }

    #[test]
    fn worker_adapter_kind_parse_and_supported() {
        let all = [
            WorkerAdapterKind::Octoscode,
            WorkerAdapterKind::Fake,
            WorkerAdapterKind::Cmd,
            WorkerAdapterKind::Claude,
            WorkerAdapterKind::Codex,
            WorkerAdapterKind::Acp,
        ];
        for kind in all {
            assert_eq!(WorkerAdapterKind::parse(kind.as_str()), Some(kind));
            // `is_supported` matches every current variant, including octoscode.
            assert!(kind.is_supported(), "{}", kind.as_str());
        }
        assert_eq!(WorkerAdapterKind::parse("not_a_kind"), None);
        assert_eq!(WorkerAdapterKind::parse(""), None);
    }

    #[test]
    fn verdict_decision_parse_roundtrip() {
        let all = [
            VerdictDecision::Accept,
            VerdictDecision::Reject,
            VerdictDecision::Rework,
            VerdictDecision::Escalate,
        ];
        for decision in all {
            assert_eq!(VerdictDecision::parse(decision.as_str()), Some(decision));
        }
        assert_eq!(VerdictDecision::parse("accepted"), None);
    }

    #[test]
    fn assignee_role_parse_roundtrip() {
        let all = [AssigneeRole::Master, AssigneeRole::Goal, AssigneeRole::Peer];
        for role in all {
            assert_eq!(AssigneeRole::parse(role.as_str()), Some(role));
        }
        assert_eq!(AssigneeRole::parse("worker"), None);
    }

    #[test]
    fn inner_and_outer_branch_names_embed_run_id() {
        let run_id = "wx_01M5HFJB29MR7QWJ2AV9HDSVPN";
        let inner = inner_branch_name(run_id);
        let outer = outer_branch_name(run_id);
        assert!(inner.contains(run_id));
        assert!(outer.contains(run_id));
        assert_eq!(inner, "wanax/wx_01M5HFJB29MR7QWJ2AV9HDSVPN/inner");
        assert_eq!(outer, "wanax/wx_01M5HFJB29MR7QWJ2AV9HDSVPN/outer");
    }
}
