use std::fmt;

/// Stable error codes. One semantic, one string. Do not alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    NotGit,
    AlreadyInit,
    ContractInvalid,
    DirtyWorktree,
    RepoLocked,
    RunNotFound,
    AdapterMissing,
    MissingApiKey,
    Db,
    TestCommandForbidden,
    WorkerCrash,
    WorkerTimeout,
    OuterTestTimeout,
    Boundary,
    AcceptOverride,
    CommanderSchema,
    Budget,
    ReworkLimit,
    ContractMutated,
    PushAttempt,
    PeerOverlap,
    LockStale,
    ProtectedRef,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotGit => "E_NOT_GIT",
            Self::AlreadyInit => "E_ALREADY_INIT",
            Self::ContractInvalid => "E_CONTRACT_INVALID",
            Self::DirtyWorktree => "E_DIRTY_WORKTREE",
            Self::RepoLocked => "E_REPO_LOCKED",
            Self::RunNotFound => "E_RUN_NOT_FOUND",
            Self::AdapterMissing => "E_ADAPTER_MISSING",
            Self::MissingApiKey => "E_MISSING_API_KEY",
            Self::Db => "E_DB",
            Self::TestCommandForbidden => "E_TEST_COMMAND_FORBIDDEN",
            Self::WorkerCrash => "E_WORKER_CRASH",
            Self::WorkerTimeout => "E_WORKER_TIMEOUT",
            Self::OuterTestTimeout => "E_OUTER_TEST_TIMEOUT",
            Self::Boundary => "E_BOUNDARY",
            Self::AcceptOverride => "E_ACCEPT_OVERRIDE",
            Self::CommanderSchema => "E_COMMANDER_SCHEMA",
            Self::Budget => "E_BUDGET",
            Self::ReworkLimit => "E_REWORK_LIMIT",
            Self::ContractMutated => "E_CONTRACT_MUTATED",
            Self::PushAttempt => "E_PUSH_ATTEMPT",
            Self::PeerOverlap => "E_PEER_OVERLAP",
            Self::LockStale => "E_LOCK_STALE",
            Self::ProtectedRef => "E_PROTECTED_REF",
        }
    }

    pub const fn exit_code(self) -> i32 {
        match self {
            Self::NotGit => 2,
            Self::AlreadyInit => 3,
            Self::ContractInvalid | Self::TestCommandForbidden => 4,
            Self::DirtyWorktree => 5,
            Self::RepoLocked => 6,
            Self::RunNotFound => 7,
            Self::AdapterMissing | Self::MissingApiKey => 8,
            Self::Db => 9,
            _ => 1,
        }
    }

    pub const fn default_message(self) -> &'static str {
        match self {
            Self::NotGit => "not a git repository",
            Self::AlreadyInit => "already initialized",
            Self::ContractInvalid => "invalid contract",
            Self::DirtyWorktree => "dirty worktree; commit, stash, or pass --allow-dirty",
            Self::RepoLocked => "repo locked by run",
            Self::RunNotFound => "run not found",
            Self::AdapterMissing => "adapter binary not found",
            Self::MissingApiKey => "missing WANAX_COMMANDER_API_KEY",
            Self::Db => "database error",
            Self::TestCommandForbidden => "test_command rejected",
            Self::WorkerCrash => "worker crashed without receipt",
            Self::WorkerTimeout => "worker timeout",
            Self::OuterTestTimeout => "outer test timeout",
            Self::Boundary => "boundary check failed",
            Self::AcceptOverride => "accept overridden; tests did not pass",
            Self::CommanderSchema => "commander schema invalid",
            Self::Budget => "budget exhausted",
            Self::ReworkLimit => "max rework exceeded",
            Self::ContractMutated => "contract mutated on disk; run still uses frozen hash",
            Self::PushAttempt => "push denied",
            Self::PeerOverlap => "peer file sets overlap",
            Self::LockStale => "stale lock",
            Self::ProtectedRef => "protected ref",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct WanaxError {
    pub code: ErrorCode,
    pub message: String,
}

impl WanaxError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn from_code(code: ErrorCode) -> Self {
        Self::new(code, code.default_message())
    }

    pub fn with_detail(code: ErrorCode, detail: impl fmt::Display) -> Self {
        Self::new(code, format!("{}: {detail}", code.default_message()))
    }

    pub fn exit_code(&self) -> i32 {
        self.code.exit_code()
    }

    pub fn stderr_line(&self) -> String {
        format!("ERROR {} {}", self.code.as_str(), self.message)
    }
}

impl fmt::Display for WanaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for WanaxError {}

impl From<sqlx::Error> for WanaxError {
    fn from(err: sqlx::Error) -> Self {
        Self::with_detail(ErrorCode::Db, err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn error_code_strings_are_unique() {
        let codes = [
            ErrorCode::NotGit,
            ErrorCode::AlreadyInit,
            ErrorCode::ContractInvalid,
            ErrorCode::DirtyWorktree,
            ErrorCode::RepoLocked,
            ErrorCode::RunNotFound,
            ErrorCode::AdapterMissing,
            ErrorCode::MissingApiKey,
            ErrorCode::Db,
            ErrorCode::TestCommandForbidden,
            ErrorCode::WorkerCrash,
            ErrorCode::WorkerTimeout,
            ErrorCode::OuterTestTimeout,
            ErrorCode::Boundary,
            ErrorCode::AcceptOverride,
            ErrorCode::CommanderSchema,
            ErrorCode::Budget,
            ErrorCode::ReworkLimit,
            ErrorCode::ContractMutated,
            ErrorCode::PushAttempt,
            ErrorCode::PeerOverlap,
            ErrorCode::LockStale,
            ErrorCode::ProtectedRef,
        ];
        let mut seen = HashSet::new();
        for c in codes {
            assert!(seen.insert(c.as_str()), "duplicate {}", c.as_str());
        }
    }

    #[test]
    fn catalog_exit_codes_match_spec() {
        assert_eq!(ErrorCode::NotGit.exit_code(), 2);
        assert_eq!(ErrorCode::AlreadyInit.exit_code(), 3);
        assert_eq!(ErrorCode::ContractInvalid.exit_code(), 4);
        assert_eq!(ErrorCode::TestCommandForbidden.exit_code(), 4);
        assert_eq!(ErrorCode::DirtyWorktree.exit_code(), 5);
        assert_eq!(ErrorCode::RepoLocked.exit_code(), 6);
        assert_eq!(ErrorCode::RunNotFound.exit_code(), 7);
        assert_eq!(ErrorCode::AdapterMissing.exit_code(), 8);
        assert_eq!(ErrorCode::MissingApiKey.exit_code(), 8);
        assert_eq!(ErrorCode::Db.exit_code(), 9);
    }
}
