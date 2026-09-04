use crate::error::{ErrorCode, WanaxError};
use crate::types::RunState;

/// Legal transitions for FactoryRun.state.
pub fn can_transition(from: RunState, to: RunState) -> bool {
    if from == to {
        return true;
    }
    if from.is_terminal() {
        return false;
    }
    match (from, to) {
        (RunState::Draft, RunState::ContractReady) => true,
        (RunState::ContractReady, RunState::Dispatched) => true,
        (RunState::Dispatched, RunState::InnerWorking) => true,
        (RunState::InnerWorking, RunState::ReceiptReady) => true,
        (RunState::ReceiptReady, RunState::OuterReviewing) => true,
        (
            RunState::OuterReviewing,
            RunState::Accepted
                | RunState::Rejected
                | RunState::Rework
                | RunState::Escalate
                | RunState::Dispatched,
        ) => true,
        (RunState::Rework, RunState::Dispatched) => true,
        (_, RunState::Canceling) => !from.is_terminal(),
        (RunState::Canceling, RunState::Cancelled) => true,
        (_, RunState::BudgetExhausted) => !from.is_terminal(),
        (_, RunState::Failed) => !from.is_terminal(),
        (_, RunState::Escalate) => !from.is_terminal(),
        _ => false,
    }
}

pub fn transition(from: RunState, to: RunState) -> Result<RunState, WanaxError> {
    if can_transition(from, to) {
        Ok(to)
    } else {
        Err(WanaxError::new(
            ErrorCode::Db,
            format!(
                "illegal state transition {} → {}",
                from.as_str(),
                to.as_str()
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_transitions() {
        let path = [
            RunState::Draft,
            RunState::ContractReady,
            RunState::Dispatched,
            RunState::InnerWorking,
            RunState::ReceiptReady,
            RunState::OuterReviewing,
            RunState::Accepted,
        ];
        for w in path.windows(2) {
            assert!(can_transition(w[0], w[1]), "{:?} → {:?}", w[0], w[1]);
        }
    }

    #[test]
    fn terminal_cannot_leave() {
        assert!(!can_transition(RunState::Accepted, RunState::Dispatched));
        assert!(!can_transition(RunState::Failed, RunState::Canceling));
    }

    #[test]
    fn rework_returns_to_dispatched() {
        assert!(can_transition(RunState::Rework, RunState::Dispatched));
        assert!(can_transition(RunState::OuterReviewing, RunState::Rework));
        assert!(can_transition(RunState::OuterReviewing, RunState::Dispatched));
    }
}
