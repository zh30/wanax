use crate::error::{ErrorCode, WanaxError};
use crate::money::estimate_cost_micros;
use crate::types::FactoryRun;

#[derive(Debug, Clone)]
pub struct BudgetTick {
    pub spent_usd_micros: i64,
    pub spent_inner_turns: u32,
    pub cost_estimated: bool,
    pub exhausted: bool,
}

pub fn would_exhaust_usd(run: &FactoryRun, additional: i64) -> bool {
    run.spent_usd_micros.saturating_add(additional) >= run.max_usd_micros
}

pub fn is_usd_exhausted(run: &FactoryRun) -> bool {
    run.spent_usd_micros >= run.max_usd_micros
}

pub fn is_turns_exhausted(run: &FactoryRun) -> bool {
    run.spent_inner_turns >= run.max_inner_turns
}

pub fn is_budget_exhausted(run: &FactoryRun) -> bool {
    is_usd_exhausted(run) || is_turns_exhausted(run)
}

pub fn budget_error(run: &FactoryRun) -> WanaxError {
    WanaxError::new(
        ErrorCode::Budget,
        format!(
            "budget exhausted usd={} turns={}",
            crate::money::format_usd_4(run.spent_usd_micros),
            run.spent_inner_turns
        ),
    )
}

pub fn charge_estimated(
    run: &mut FactoryRun,
    chars_in: u64,
    chars_out: u64,
    usd_per_million_in: i64,
    usd_per_million_out: i64,
) -> BudgetTick {
    let add = estimate_cost_micros(chars_in, chars_out, usd_per_million_in, usd_per_million_out);
    run.spent_usd_micros = run.spent_usd_micros.saturating_add(add);
    BudgetTick {
        spent_usd_micros: run.spent_usd_micros,
        spent_inner_turns: run.spent_inner_turns,
        cost_estimated: true,
        exhausted: is_budget_exhausted(run),
    }
}

pub fn add_turns(run: &mut FactoryRun, n: u32) -> BudgetTick {
    run.spent_inner_turns = run.spent_inner_turns.saturating_add(n);
    BudgetTick {
        spent_usd_micros: run.spent_usd_micros,
        spent_inner_turns: run.spent_inner_turns,
        cost_estimated: false,
        exhausted: is_budget_exhausted(run),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{RunState, WorkerAdapterKind};

    fn run(max_usd: i64, max_turns: u32) -> FactoryRun {
        FactoryRun {
            id: "wx_01AAAAAAAAAAAAAAAAAAAAAAAA".into(),
            repo_root: "/tmp".into(),
            contract_id: "wx_01BBBBBBBBBBBBBBBBBBBBBBBB".into(),
            contract_sha256: "aa".repeat(32),
            state: RunState::Dispatched,
            base_sha: "a".repeat(40),
            inner_branch: "wanax/x/inner".into(),
            outer_branch: "wanax/x/outer".into(),
            commander_model: "c".into(),
            inner_model: "i".into(),
            reviewer_model: None,
            max_usd_micros: max_usd,
            max_inner_turns: max_turns,
            spent_usd_micros: 0,
            spent_inner_turns: 0,
            worker_adapter: WorkerAdapterKind::Fake,
            created_at: String::new(),
            updated_at: String::new(),
            finished_at: None,
            last_error: None,
            worker_pid: None,
            start_pid: None,
        }
    }

    #[test]
    fn zero_usd_budget_is_exhausted() {
        let r = run(0, 40);
        assert!(is_usd_exhausted(&r));
    }

    #[test]
    fn turns_at_max_without_receipt_is_exhausted() {
        let mut r = run(5_000_000, 40);
        add_turns(&mut r, 40);
        assert!(is_turns_exhausted(&r));
        let mut r39 = run(5_000_000, 40);
        add_turns(&mut r39, 39);
        assert!(!is_turns_exhausted(&r39));
    }
}
