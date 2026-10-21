//! `wanax report` — mine run tombstones into a fitness view.
//!
//! This is the measurement substrate for RSI iteration: accept rate, rework,
//! cost, wall time, and failure-mode histograms, all read from the
//! append-only envelope (with the DB row for authoritative totals).

use std::collections::HashMap;
use std::path::PathBuf;
use wanax_core::error::WanaxError;
use wanax_core::money::format_usd_4;
use wanax_core::timeutil::parse_rfc3339;
use wanax_core::types::{FactoryRun, RunState};
use wanax_core::Store;
use wanax_tombstone::{load_envelope, EventKind};

#[derive(Default)]
struct Fitness {
    baseline_exit: Option<i64>,
    outer_exits: Vec<i64>,
    verdicts: Vec<String>,
    verdict_reasons: Vec<String>,
    verdict_baseline: Option<i64>,
    boundary_paths: Vec<String>,
    error_codes: Vec<String>,
    events: usize,
    wall_ms: i64,
    sandbox: Option<String>,
    iteration: Option<u64>,
    parent_run_id: Option<String>,
}

fn mine(repo_root: &std::path::Path, run_id: &str) -> Fitness {
    let mut f = Fitness::default();
    let Ok(env) = load_envelope(repo_root, run_id) else {
        return f;
    };
    f.events = env.events.len();
    if let (Some(first), Some(last)) = (env.events.first(), env.events.last()) {
        if let (Ok(a), Ok(b)) = (parse_rfc3339(&first.at), parse_rfc3339(&last.at)) {
            f.wall_ms = (b - a).num_milliseconds().max(0);
        }
    }
    let mut errors: HashMap<String, u32> = HashMap::new();
    for e in &env.events {
        match e.kind {
            EventKind::BaselineTest => {
                if f.baseline_exit.is_none() {
                    f.baseline_exit = e.payload["exit_code"].as_i64();
                }
            }
            EventKind::OuterTestFinished => {
                if let Some(c) = e.payload["exit_code"].as_i64() {
                    f.outer_exits.push(c);
                }
            }
            EventKind::Verdict => {
                if let Some(d) = e.payload["decision"].as_str() {
                    f.verdicts.push(d.to_string());
                }
                if let Some(r) = e.payload["reason"].as_str() {
                    f.verdict_reasons.push(r.to_string());
                }
                if let Some(b) = e.payload["baseline_exit"].as_i64() {
                    f.verdict_baseline = Some(b);
                }
            }
            EventKind::Error => {
                if let Some(c) = e.payload["code"].as_str() {
                    *errors.entry(c.to_string()).or_default() += 1;
                }
                if let Some(paths) = e.payload["paths"].as_array() {
                    for p in paths {
                        if let Some(s) = p.as_str() {
                            f.boundary_paths.push(s.to_string());
                        }
                    }
                }
            }
            EventKind::RunStarted => {
                f.sandbox = e.payload["sandbox"].as_str().map(String::from);
                f.iteration = e.payload["iteration"].as_u64();
                f.parent_run_id = e.payload["parent_run_id"].as_str().map(String::from);
            }
            _ => {}
        }
    }
    f.error_codes = errors.iter().map(|(k, v)| format!("{k}x{v}")).collect();
    f.error_codes.sort();
    f
}

/// Walk the `parent_run_id` chain recorded in each run_started event —
/// the auditable lineage of an improve loop (newest first).
fn parent_chain(repo_root: &std::path::Path, run_id: &str) -> Vec<String> {
    let mut chain = Vec::new();
    let mut cur = Some(run_id.to_string());
    for _ in 0..8 {
        let Some(id) = cur else { break };
        chain.push(id.clone());
        cur = load_envelope(repo_root, &id)
            .ok()
            .and_then(|env| {
                env.events
                    .iter()
                    .find(|e| e.kind == EventKind::RunStarted)
                    .and_then(|e| e.payload["parent_run_id"].as_str().map(String::from))
            });
        // Guard against self-referencing cycles.
        if cur.as_deref() == Some(id.as_str()) {
            break;
        }
    }
    chain
}

fn print_detail(run: &FactoryRun, f: &Fitness, repo_root: &std::path::Path) {
    println!("run_id={}", run.id);
    println!("state={}", run.state.as_str());
    println!("adapter={}", run.worker_adapter.as_str());
    if let (Some(it), Some(p)) = (f.iteration, &f.parent_run_id) {
        println!("iteration={it} parent_run_id={p}");
    }
    let chain = parent_chain(repo_root, &run.id);
    if chain.len() > 1 {
        println!("chain={}", chain.join(" <- "));
    }
    if let Some(sb) = &f.sandbox {
        println!("sandbox={sb}");
    }
    match f.baseline_exit {
        Some(c) => println!("baseline_exit={c}"),
        None => println!("baseline_exit=-"),
    }
    // Accepted on an already-green baseline → possibly vacuous; flag it.
    let baseline_green = f
        .verdict_baseline
        .or(f.baseline_exit)
        .map(|b| b == 0);
    if let Some(g) = baseline_green {
        println!("baseline_green={g}");
    }
    println!(
        "outer_test_exits={}",
        if f.outer_exits.is_empty() {
            "-".to_string()
        } else {
            format!("{:?}", f.outer_exits)
        }
    );
    println!(
        "verdicts={}",
        if f.verdicts.is_empty() {
            "-".to_string()
        } else {
            f.verdicts.join(" -> ")
        }
    );
    if let Some(reason) = f.verdict_reasons.last() {
        println!("last_verdict_reason={reason}");
    }
    if !f.boundary_paths.is_empty() {
        println!("boundary_violations={}", f.boundary_paths.join(","));
    }
    println!(
        "errors={}",
        if f.error_codes.is_empty() {
            "-".to_string()
        } else {
            f.error_codes.join(",")
        }
    );
    println!(
        "spent_usd={} spent_turns={}",
        format_usd_4(run.spent_usd_micros),
        run.spent_inner_turns
    );
    println!("wall_s={} events={}", f.wall_ms / 1000, f.events);
}

pub async fn run(
    data_dir: PathBuf,
    run_id: Option<String>,
    json_out: bool,
    suggest: bool,
) -> Result<(), WanaxError> {
    let store = Store::open(&data_dir.join("wanax.db")).await?;
    match run_id {
        Some(id) => {
            let run = store.get_run(&id).await?;
            let root = std::path::Path::new(&run.repo_root);
            let f = mine(root, &run.id);
            if json_out {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "run_id": run.id,
                        "state": run.state.as_str(),
                        "adapter": run.worker_adapter.as_str(),
                        "iteration": f.iteration,
                        "parent_run_id": f.parent_run_id,
                        "chain": parent_chain(root, &run.id),
                        "sandbox": f.sandbox,
                        "baseline_exit": f.baseline_exit,
                        "outer_test_exits": f.outer_exits,
                        "verdicts": f.verdicts,
                        "error_codes": f.error_codes,
                        "boundary_violations": f.boundary_paths,
                        "spent_usd_micros": run.spent_usd_micros,
                        "spent_inner_turns": run.spent_inner_turns,
                        "wall_ms": f.wall_ms,
                        "events": f.events,
                    }))
                    .unwrap_or_else(|_| "{}".into())
                );
            } else {
                print_detail(&run, &f, root);
            }
        }
        None => {
            let runs = store.list_runs().await?;
            if runs.is_empty() {
                println!("{}", crate::i18n::t("no_runs"));
                return Ok(());
            }
            let mut by_state: HashMap<String, u32> = HashMap::new();
            let mut errors: HashMap<String, u32> = HashMap::new();
            // Per-contract fitness + failure-mode stats — RSI's actual
            // question is "is this spec converging, and why not".
            #[derive(Default)]
            struct CStats {
                finished: u32,
                accepted: u32,
                reworks: u32,
                boundary: u32,
                budget: u32,
                vacuous_accepts: u32,
                commander_schema: u32,
                history: String,
            }
            let mut by_contract: HashMap<String, CStats> = HashMap::new();
            let mut n_finished = 0u32;
            let mut n_accepted = 0u32;
            let mut sum_usd = 0i64;
            let mut sum_turns = 0u64;
            let mut sum_wall_ms = 0i64;
            // Oldest → newest so per-contract history reads as a timeline.
            for run in runs.iter().rev() {
                *by_state.entry(run.state.as_str().to_string()).or_default() += 1;
                let f = mine(std::path::Path::new(&run.repo_root), &run.id);
                for c in &f.error_codes {
                    let (code, n) = c.split_once('x').unwrap_or((c.as_str(), "1"));
                    *errors.entry(code.to_string()).or_default() +=
                        n.parse::<u32>().unwrap_or(1);
                }
                if run.state.is_terminal() {
                    n_finished += 1;
                    if run.state == RunState::Accepted {
                        n_accepted += 1;
                    }
                    sum_usd += run.spent_usd_micros;
                    sum_turns += u64::from(run.spent_inner_turns);
                    sum_wall_ms += f.wall_ms;
                    let label = store
                        .get_contract(&run.contract_id)
                        .await
                        .ok()
                        .and_then(|c| c.name)
                        .unwrap_or_else(|| run.contract_sha256.chars().take(8).collect());
                    let e = by_contract.entry(label).or_default();
                    e.finished += 1;
                    e.history.push(match run.state {
                        RunState::Accepted => 'A',
                        RunState::Rejected => 'R',
                        RunState::Escalate => 'E',
                        RunState::Cancelled => 'C',
                        _ => 'F',
                    });
                    if run.state == RunState::Accepted {
                        e.accepted += 1;
                        if f.verdict_baseline.or(f.baseline_exit) == Some(0) {
                            e.vacuous_accepts += 1;
                        }
                    }
                    e.reworks += f
                        .verdicts
                        .iter()
                        .filter(|v| v.as_str() == "rework")
                        .count() as u32;
                    if !f.boundary_paths.is_empty() {
                        e.boundary += 1;
                    }
                    if f.error_codes.iter().any(|c| c.starts_with("E_BUDGET")) {
                        e.budget += 1;
                    }
                    if f.error_codes
                        .iter()
                        .any(|c| c.starts_with("E_COMMANDER_SCHEMA"))
                    {
                        e.commander_schema += 1;
                    }
                }
            }
            if json_out {
                let contracts: Vec<_> = by_contract
                    .iter()
                    .map(|(name, s)| {
                        serde_json::json!({
                            "contract": name,
                            "finished": s.finished,
                            "accepted": s.accepted,
                            "reworks": s.reworks,
                            "boundary_violation_runs": s.boundary,
                            "budget_failures": s.budget,
                            "vacuous_accepts": s.vacuous_accepts,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "runs": runs.len(),
                        "by_state": by_state,
                        "finished": n_finished,
                        "accepted": n_accepted,
                        "accept_rate": if n_finished > 0 {
                            Some(n_accepted as f64 / n_finished as f64)
                        } else { None },
                        "avg_usd_micros": if n_finished > 0 {
                            Some(sum_usd / i64::from(n_finished))
                        } else { None },
                        "avg_turns": if n_finished > 0 {
                            Some(sum_turns as f64 / f64::from(n_finished))
                        } else { None },
                        "avg_wall_ms": if n_finished > 0 {
                            Some(sum_wall_ms / i64::from(n_finished))
                        } else { None },
                        "top_errors": errors,
                        "contracts": contracts,
                    }))
                    .unwrap_or_else(|_| "{}".into())
                );
                return Ok(());
            }
            let mut states: Vec<String> =
                by_state.iter().map(|(k, v)| format!("{k}={v}")).collect();
            states.sort();
            println!("runs={} {}", runs.len(), states.join(" "));
            if n_finished > 0 {
                println!(
                    "accept_rate={:.2} (of {} finished)",
                    n_accepted as f64 / n_finished as f64,
                    n_finished
                );
                println!(
                    "avg_usd={} avg_turns={:.1} avg_wall_s={}",
                    format_usd_4(sum_usd / i64::from(n_finished)),
                    sum_turns as f64 / f64::from(n_finished),
                    sum_wall_ms / 1000 / i64::from(n_finished),
                );
            }
            if !errors.is_empty() {
                let mut list: Vec<(&String, &u32)> = errors.iter().collect();
                list.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
                let top = list
                    .iter()
                    .take(6)
                    .map(|(k, v)| format!("{k}x{v}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!("top_errors: {top}");
            }
            let mut contracts: Vec<String> = by_contract
                .iter()
                .map(|(k, s)| {
                    format!("{k}: {}/{} accepted [{}]", s.accepted, s.finished, s.history)
                })
                .collect();
            contracts.sort();
            for c in contracts {
                println!("contract {c}");
            }
            if suggest {
                let mut hints = Vec::new();
                for (name, s) in &by_contract {
                    if s.commander_schema > 0 {
                        hints.push(format!(
                            "{name}: commander output failed {}x — prefer a structured provider (json_mode) or claude_cli envelope",
                            s.commander_schema
                        ));
                    }
                    if s.boundary > 0 {
                        hints.push(format!(
                            "{name}: boundary violations in {} run(s) — allowed_globs may be too tight, or the instruction drifts past declared scope",
                            s.boundary
                        ));
                    }
                    if s.budget > 0 {
                        hints.push(format!(
                            "{name}: budget exhausted in {} run(s) — raise max_usd or split the intent into smaller units",
                            s.budget
                        ));
                    }
                    if s.vacuous_accepts > 0 {
                        hints.push(format!(
                            "{name}: {} accept(s) on an already-green baseline — completion criteria may be vacuous; require a test that flips red→green",
                            s.vacuous_accepts
                        ));
                    }
                    if s.accepted == 0 && s.reworks >= 3 * s.finished.max(1) {
                        hints.push(format!(
                            "{name}: heavy rework with zero accepts — the intent is likely ambiguous; narrow decisions and completion criteria"
                        ));
                    }
                }
                if hints.is_empty() {
                    println!("suggest: no recurring failure pattern detected");
                } else {
                    hints.sort();
                    for hint in hints {
                        println!("suggest {hint}");
                    }
                }
            }
        }
    }
    Ok(())
}
