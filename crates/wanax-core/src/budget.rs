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

pub fn charge_units(
    run: &mut FactoryRun,
    units_in: u64,
    units_out: u64,
    usd_per_million_in: i64,
    usd_per_million_out: i64,
    cost_estimated: bool,
) -> BudgetTick {
    let add = estimate_cost_micros(units_in, units_out, usd_per_million_in, usd_per_million_out);
    run.spent_usd_micros = run.spent_usd_micros.saturating_add(add);
    BudgetTick {
        spent_usd_micros: run.spent_usd_micros,
        spent_inner_turns: run.spent_inner_turns,
        cost_estimated,
        exhausted: is_budget_exhausted(run),
    }
}

pub fn charge_estimated(
    run: &mut FactoryRun,
    chars_in: u64,
    chars_out: u64,
    usd_per_million_in: i64,
    usd_per_million_out: i64,
) -> BudgetTick {
    charge_units(
        run,
        chars_in,
        chars_out,
        usd_per_million_in,
        usd_per_million_out,
        true,
    )
}

/// Charge real USD reported by a worker CLI (e.g. Claude `total_cost_usd`).
pub fn charge_usd_micros(run: &mut FactoryRun, usd_micros: i64) -> BudgetTick {
    run.spent_usd_micros = run.spent_usd_micros.saturating_add(usd_micros);
    BudgetTick {
        spent_usd_micros: run.spent_usd_micros,
        spent_inner_turns: run.spent_inner_turns,
        cost_estimated: false,
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

    #[test]
    fn charge_units_marks_estimated() {
        let mut r = run(5_000_000, 40);
        let estimated = charge_units(&mut r, 1_000_000, 0, 10_000_000, 50_000_000, true);
        assert!(estimated.cost_estimated);
        assert_eq!(estimated.spent_usd_micros, 10_000_000);
        let real = charge_units(&mut r, 10, 0, 10_000_000, 50_000_000, false);
        assert!(!real.cost_estimated);
    }

    #[test]
    fn budget_exhaustion_boundaries() {
        // would_exhaust_usd: spent + additional >= max. Strictly below is false;
        // landing exactly on the cap is true (not cap+1); crossing is true.
        let cap = 1_000_000i64;
        let fresh = run(cap, 10);
        assert!(!would_exhaust_usd(&fresh, cap - 1));
        assert!(would_exhaust_usd(&fresh, cap));
        assert!(would_exhaust_usd(&fresh, cap + 1));

        let mut almost = run(cap, 10);
        almost.spent_usd_micros = cap - 1;
        assert!(!would_exhaust_usd(&almost, 0));
        assert!(would_exhaust_usd(&almost, 1));
        assert!(would_exhaust_usd(&almost, 2));

        let mut spent = run(cap, 10);
        spent.spent_usd_micros = cap;
        assert!(would_exhaust_usd(&spent, 0));

        // is_usd_exhausted: spent >= max. One micro below is live; equal and above are exhausted.
        let mut usd = run(cap, 10);
        usd.spent_usd_micros = cap - 1;
        assert!(!is_usd_exhausted(&usd));
        assert!(!is_budget_exhausted(&usd));
        usd.spent_usd_micros = cap;
        assert!(is_usd_exhausted(&usd));
        assert!(is_budget_exhausted(&usd));
        usd.spent_usd_micros = cap + 1;
        assert!(is_usd_exhausted(&usd));
        assert!(is_budget_exhausted(&usd));

        // is_turns_exhausted: spent_inner_turns >= max. One turn below is live.
        let mut turns = run(cap, 10);
        turns.spent_inner_turns = 9;
        assert!(!is_turns_exhausted(&turns));
        assert!(!is_budget_exhausted(&turns));
        turns.spent_inner_turns = 10;
        assert!(is_turns_exhausted(&turns));
        assert!(is_budget_exhausted(&turns));
        turns.spent_inner_turns = 11;
        assert!(is_turns_exhausted(&turns));
        assert!(is_budget_exhausted(&turns));

        // Either dimension alone exhausts the budget while the other is still under its cap.
        let mut usd_only = run(cap, 10);
        usd_only.spent_usd_micros = cap;
        assert!(is_usd_exhausted(&usd_only));
        assert!(!is_turns_exhausted(&usd_only));
        assert!(is_budget_exhausted(&usd_only));
        let mut turns_only = run(cap, 10);
        turns_only.spent_inner_turns = 10;
        assert!(!is_usd_exhausted(&turns_only));
        assert!(is_turns_exhausted(&turns_only));
        assert!(is_budget_exhausted(&turns_only));

        // charge_usd_micros accumulates; the tick reports post-charge spend, not pre-charge.
        let mut charged = run(cap, 10);
        let t1 = charge_usd_micros(&mut charged, 100);
        assert_eq!(t1.spent_usd_micros, 100);
        assert_eq!(charged.spent_usd_micros, 100);
        assert!(!t1.cost_estimated);
        assert!(!t1.exhausted);
        let t2 = charge_usd_micros(&mut charged, 250);
        assert_eq!(t2.spent_usd_micros, 350);
        assert_eq!(charged.spent_usd_micros, 350);
        assert_eq!(t2.spent_inner_turns, 0);

        // add_turns accumulates; the tick reports post-charge turns.
        let a1 = add_turns(&mut charged, 3);
        assert_eq!(a1.spent_inner_turns, 3);
        assert_eq!(charged.spent_inner_turns, 3);
        assert_eq!(a1.spent_usd_micros, 350);
        assert!(!a1.cost_estimated);
        let a2 = add_turns(&mut charged, 4);
        assert_eq!(a2.spent_inner_turns, 7);
        assert_eq!(charged.spent_inner_turns, 7);
        // Crossing the turn cap is visible on the returned tick, not only on a later read.
        let a3 = add_turns(&mut charged, 3);
        assert_eq!(a3.spent_inner_turns, 10);
        assert!(a3.exhausted);

        // charge_estimated projects char counts into USD and flags the tick estimated.
        // charge_units with cost_estimated=false books the same formula as real usage.
        // Swapping the flag (or skipping the projection) fails these asserts.
        // 500_000 chars * 2_000_000 micros/1M = 1_000_000 micros exactly.
        let mut projected = run(5_000_000, 40);
        let est = charge_estimated(&mut projected, 500_000, 0, 2_000_000, 0);
        assert!(est.cost_estimated);
        assert_eq!(est.spent_usd_micros, 1_000_000);
        assert_eq!(projected.spent_usd_micros, 1_000_000);
        assert!(!est.exhausted);

        let booked = charge_units(&mut projected, 500_000, 0, 2_000_000, 0, false);
        assert!(!booked.cost_estimated);
        assert_eq!(booked.spent_usd_micros, 2_000_000);
        assert_eq!(projected.spent_usd_micros, 2_000_000);

        // budget_error is E_BUDGET and names both dimensions; the usd figure is the
        // 4-decimal formatting of spent micros, and turns is the raw spent count.
        let mut err_run = run(cap, 10);
        err_run.spent_usd_micros = 1_500_000;
        err_run.spent_inner_turns = 7;
        let err = budget_error(&err_run);
        assert_eq!(err.code, ErrorCode::Budget);
        assert_eq!(err.code.as_str(), "E_BUDGET");
        let msg = err.to_string();
        assert!(
            msg.contains("usd=1.5000"),
            "usd dimension missing from {msg}"
        );
        assert!(msg.contains("turns=7"), "turns dimension missing from {msg}");

        let mut turns_err = run(cap, 10);
        turns_err.spent_usd_micros = 0;
        turns_err.spent_inner_turns = 10;
        let terr = budget_error(&turns_err);
        assert_eq!(terr.code, ErrorCode::Budget);
        let tmsg = terr.to_string();
        assert!(
            tmsg.contains("usd=0.0000"),
            "zero usd not named in {tmsg}"
        );
        assert!(
            tmsg.contains("turns=10"),
            "exhausted turns not named in {tmsg}"
        );
    }
}
