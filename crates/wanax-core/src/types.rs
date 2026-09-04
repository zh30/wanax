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
}

impl WorkerAdapterKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Octoscode => "octoscode",
            Self::Fake => "fake",
            Self::Cmd => "cmd",
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "octoscode" => Self::Octoscode,
            "fake" => Self::Fake,
            "cmd" => Self::Cmd,
            "claude" => Self::Claude,
            "codex" => Self::Codex,
            _ => return None,
        })
    }

    pub fn is_phase1(self) -> bool {
        matches!(self, Self::Octoscode | Self::Fake | Self::Cmd)
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

    #[test]
    fn rework_is_not_terminal() {
        assert!(!RunState::Rework.is_terminal());
        assert!(RunState::Escalate.is_terminal());
        assert!(RunState::Accepted.is_terminal());
    }
}
