use crate::error::ErrorCode;
use crate::types::{Receipt, VerdictDecision, MAX_REWORK};

#[derive(Debug, Clone)]
pub struct AcceptGates {
    pub outer_test_exit_code: i32,
    pub boundary_ok: bool,
    pub receipt_test_command: String,
    pub contract_test_command: String,
    pub inner_is_descendant: bool,
    pub changed_files_nonempty: bool,
    pub budget_exhausted: bool,
    pub rework_count: u32,
}

impl AcceptGates {
    pub fn from_parts(
        receipt: &Receipt,
        contract_test_command: &str,
        outer_test_exit_code: i32,
        boundary_ok: bool,
        inner_is_descendant: bool,
        budget_exhausted: bool,
        rework_count: u32,
    ) -> Self {
        Self {
            outer_test_exit_code,
            boundary_ok,
            receipt_test_command: receipt.test_command.clone(),
            contract_test_command: contract_test_command.to_string(),
            inner_is_descendant,
            changed_files_nonempty: !receipt.changed_files.is_empty(),
            budget_exhausted,
            rework_count,
        }
    }

    pub fn can_accept(&self) -> bool {
        self.outer_test_exit_code == 0
            && self.boundary_ok
            && self.receipt_test_command == self.contract_test_command
            && self.inner_is_descendant
            && self.changed_files_nonempty
            && !self.budget_exhausted
            && self.rework_count <= MAX_REWORK
    }
}

/// Enforce FR-012 / FR-014. Models cannot override a red outer test into accept.
pub fn enforce_decision(
    proposed: VerdictDecision,
    gates: &AcceptGates,
) -> (VerdictDecision, Option<ErrorCode>) {
    let mut decision = proposed;
    let mut note = None;
    if gates.outer_test_exit_code != 0 && decision == VerdictDecision::Accept {
        decision = VerdictDecision::Rework;
        note = Some(ErrorCode::AcceptOverride);
    }
    if decision == VerdictDecision::Accept && !gates.can_accept() {
        if gates.budget_exhausted {
            // caller maps to budget_exhausted run state
            return (decision, Some(ErrorCode::Budget));
        }
        if !gates.boundary_ok {
            decision = VerdictDecision::Reject;
            note = Some(note.unwrap_or(ErrorCode::Boundary));
        } else if !gates.changed_files_nonempty
            || gates.receipt_test_command != gates.contract_test_command
            || !gates.inner_is_descendant
        {
            decision = VerdictDecision::Reject;
        } else if gates.rework_count > MAX_REWORK {
            decision = VerdictDecision::Escalate;
            note = Some(ErrorCode::ReworkLimit);
        } else {
            decision = VerdictDecision::Rework;
        }
    }
    if decision == VerdictDecision::Rework && gates.rework_count >= MAX_REWORK {
        decision = VerdictDecision::Escalate;
        note = Some(ErrorCode::ReworkLimit);
    }
    (decision, note)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Receipt;

    fn receipt(pass_cmd: &str, files: &[&str]) -> Receipt {
        Receipt {
            id: "wx_01AAAAAAAAAAAAAAAAAAAAAAAA".into(),
            work_unit_id: "wx_01BBBBBBBBBBBBBBBBBBBBBBBB".into(),
            changed_files: files.iter().map(|s| (*s).to_string()).collect(),
            diffstat: "1 file".into(),
            commit_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            test_command: pass_cmd.into(),
            test_exit_code: 0,
            test_excerpt: "ok".into(),
            claimed_pass: true,
            duration_ms: 1,
            adapter: "fake".into(),
            raw_artifact_path: None,
        }
    }

    #[test]
    fn claimed_pass_does_not_affect_gates() {
        let r = receipt("cargo test", &["src/lib.rs"]);
        let gates = AcceptGates::from_parts(&r, "cargo test", 1, true, true, false, 0);
        assert!(!gates.can_accept());
        let (d, note) = enforce_decision(VerdictDecision::Accept, &gates);
        assert_eq!(d, VerdictDecision::Rework);
        assert_eq!(note, Some(ErrorCode::AcceptOverride));
    }

    #[test]
    fn all_gates_required_for_accept() {
        let r = receipt("cargo test", &["src/lib.rs"]);
        let ok = AcceptGates::from_parts(&r, "cargo test", 0, true, true, false, 0);
        assert!(ok.can_accept());
        let (d, note) = enforce_decision(VerdictDecision::Accept, &ok);
        assert_eq!(d, VerdictDecision::Accept);
        assert_eq!(note, None);
    }

    #[test]
    fn fourth_rework_escalates() {
        let r = receipt("cargo test", &["src/lib.rs"]);
        let gates = AcceptGates::from_parts(&r, "cargo test", 1, true, true, false, 3);
        let (d, note) = enforce_decision(VerdictDecision::Rework, &gates);
        assert_eq!(d, VerdictDecision::Escalate);
        assert_eq!(note, Some(ErrorCode::ReworkLimit));
    }
}
