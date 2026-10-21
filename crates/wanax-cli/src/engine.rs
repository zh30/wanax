use crate::gh::maybe_create_github_pr;
use crate::logging;
use crate::replay;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use wanax_core::accept::{enforce_decision, AcceptGates};
use wanax_core::budget::{
    add_turns, budget_error, charge_units, charge_usd_micros, is_budget_exhausted,
};
use wanax_core::config::{load_merged_config, WorkerConfig};
use wanax_core::contract::parse_contract_file;
use wanax_core::envenv;
use wanax_core::glob_overlap::peer_glob_sets_overlap;
use wanax_core::error::{ErrorCode, WanaxError};
use wanax_core::hashutil::sha256_file;
use wanax_core::ids::new_id;
use wanax_core::lock::{LockAcquire, RepoLock};
use wanax_core::sandbox::{self, SandboxSpec};
use wanax_core::timeutil::now_rfc3339;
use wanax_core::types::{
    inner_branch_name, outer_branch_name, peer_branch_name, AssigneeRole, Contract, FactoryRun,
    Receipt, RunState, Verdict, VerdictDecision, WorkUnit, WorkUnitState, WorkerAdapterKind,
    MAX_GOAL_ITERS, MAX_REWORK,
};
use wanax_core::{ResolvedConfig, Store};
use wanax_git::{
    cherry_pick, create_branch, diff_name_only, diff_stat, dirty_non_wanax, git_common_dir,
    harden_inner_worktree, head_sha, install_git_wrapper, is_ancestor, prepare_worker_gitconfig,
    ref_exists, repo_root, require_git_repo, reset_branch, status_paths, worktree_add_branch,
    worktree_add_detach,
};
use wanax_llm::{
    dispatch_with_retry, pick_commander, pick_review_client, run_self_review, Commander,
    DagUnitDraft, DispatchContext, DispatchPlan, LlmUsage, MechanicalCommander, PeerUnitDraft,
    VerdictContext, WorkUnitDraft,
};
use wanax_tombstone::{
    append_event, init_envelope, make_event, persist_envelope, run_dir, Actor, EventKind,
};
use wanax_verify::{
    allowed_globs_cover_binding_tests, check_boundaries, compile_globs, find_peer_overlap,
    run_test_command, run_verifier_plugins,
};
use wanax_worker::{
    ClaudeAdapter, CmdAdapter, CodexAdapter, FakeAdapter, OctoscodeAdapter, WorkerAdapter,
    acp::AcpAdapter,
    WorkerContext,
};

pub struct StartOpts {
    pub contract: PathBuf,
    pub allow_dirty: bool,
    pub adapter: Option<WorkerAdapterKind>,
    pub data_dir: PathBuf,
}

pub struct ResumeOpts {
    pub run_id: Option<String>,
    pub allow_dirty: bool,
    pub adapter: Option<WorkerAdapterKind>,
    pub data_dir: PathBuf,
}

pub async fn start(opts: StartOpts) -> Result<(), WanaxError> {
    let cwd = std::env::current_dir().map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    require_git_repo(&cwd)?;
    let repo = repo_root(&cwd)?;
    let repo = repo
        .canonicalize()
        .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;

    let cfg = load_merged_config(&repo, &opts.data_dir)?;
    let adapter_kind = opts.adapter.unwrap_or(cfg.adapter);
    preflight_adapter(adapter_kind, &cfg)?;

    let contract_abs = if opts.contract.is_absolute() {
        opts.contract.clone()
    } else {
        cwd.join(&opts.contract)
    };
    if !contract_abs.is_file() {
        return Err(WanaxError::new(
            ErrorCode::ContractInvalid,
            format!("invalid contract: {}", opts.contract.display()),
        ));
    }
    let rel = contract_abs
        .strip_prefix(&repo)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| opts.contract.to_string_lossy().replace('\\', "/"));
    let contract = parse_contract_file(&contract_abs, &rel)?;
    let tests_writable = allowed_globs_cover_binding_tests(&contract.allowed_globs);
    if tests_writable {
        eprintln!(
            "WARN {} {}",
            ErrorCode::ContractTestsWritable.as_str(),
            ErrorCode::ContractTestsWritable.default_message()
        );
    }

    if !opts.allow_dirty {
        let dirty = dirty_non_wanax(&repo)?;
        if !dirty.is_empty() {
            return Err(WanaxError::from_code(ErrorCode::DirtyWorktree));
        }
    }

    fs::create_dir_all(&opts.data_dir).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    let store = Store::open(&opts.data_dir.join("wanax.db")).await?;

    let run_id = new_id();
    let lock = RepoLock::acquire_with(
        &repo,
        &run_id,
        &LockAcquire {
            exclusive: cfg.file.lock.repo_exclusive,
            paths: contract.allowed_globs.clone(),
            resume: false,
        },
    )?;

    let result = run_factory(
        &store,
        &repo,
        contract,
        contract_abs,
        adapter_kind,
        &cfg,
        run_id,
        None,
    )
    .await;
    let _ = lock.release();
    result
}

pub struct ImproveOpts {
    pub contract: PathBuf,
    pub allow_dirty: bool,
    pub adapter: Option<WorkerAdapterKind>,
    pub data_dir: PathBuf,
    pub iterations: u32,
    /// Seed the loop from an existing terminal run of the SAME contract —
    /// its evidence becomes iteration-1 context and parent link.
    pub from: Option<String>,
    /// Adapter escalation ladder: iteration i uses adapters[i-1] (clamped).
    /// Lets a cheap adapter try first and a stronger one take over.
    pub adapters: Vec<WorkerAdapterKind>,
    /// Loop-level USD cap across ALL iterations (micros). Each run still has
    /// its own per-run budget; this bounds the total spend of the loop.
    pub max_total_usd_micros: Option<i64>,
}

pub const MAX_IMPROVE_ITERATIONS: u32 = 8;

/// RSI loop: every iteration is a complete, independent run — fresh run id,
/// fresh worktree, full mechanical gates. What changes between iterations is
/// only the evidence pack injected into dispatch; the contract stays frozen.
pub async fn improve(opts: ImproveOpts) -> Result<(), WanaxError> {
    let iterations = opts.iterations.clamp(1, MAX_IMPROVE_ITERATIONS);
    let cwd = std::env::current_dir().map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    require_git_repo(&cwd)?;
    let repo = repo_root(&cwd)?
        .canonicalize()
        .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    let cfg = load_merged_config(&repo, &opts.data_dir)?;
    let adapter_kind = opts.adapter.unwrap_or(cfg.adapter);
    preflight_adapter(adapter_kind, &cfg)?;
    let contract_abs = if opts.contract.is_absolute() {
        opts.contract.clone()
    } else {
        cwd.join(&opts.contract)
    };
    if !contract_abs.is_file() {
        return Err(WanaxError::new(
            ErrorCode::ContractInvalid,
            format!("invalid contract: {}", opts.contract.display()),
        ));
    }
    let rel = contract_abs
        .strip_prefix(&repo)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| opts.contract.to_string_lossy().replace('\\', "/"));
    let contract = parse_contract_file(&contract_abs, &rel)?;
    if allowed_globs_cover_binding_tests(&contract.allowed_globs) {
        eprintln!(
            "WARN {} {}",
            ErrorCode::ContractTestsWritable.as_str(),
            ErrorCode::ContractTestsWritable.default_message()
        );
    }
    if !opts.allow_dirty {
        let dirty = dirty_non_wanax(&repo)?;
        if !dirty.is_empty() {
            return Err(WanaxError::from_code(ErrorCode::DirtyWorktree));
        }
    }
    fs::create_dir_all(&opts.data_dir).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    let store = Store::open(&opts.data_dir.join("wanax.db")).await?;

    // --from: continue an evidence chain started by an earlier invocation.
    let mut parent_run_id: Option<String> = None;
    let mut notes: Option<String> = None;
    if let Some(seed_id) = &opts.from {
        let seed = store.get_run(seed_id).await?;
        if !seed.state.is_terminal() {
            return Err(WanaxError::new(
                ErrorCode::Resume,
                "improve --from requires a terminal run",
            ));
        }
        if seed.contract_sha256 != contract.content_sha256 {
            return Err(WanaxError::new(
                ErrorCode::ContractInvalid,
                "improve --from run used a different contract (sha mismatch); evidence is not portable across specs",
            ));
        }
        parent_run_id = Some(seed.id.clone());
        notes = Some(evidence_notes(&repo, &seed.id));
    }

    // Adapter escalation ladder: iteration i takes adapters[i-1] clamped to
    // the last entry; a lone --adapter (or config default) means uniform.
    let ladder: Vec<WorkerAdapterKind> = if opts.adapters.is_empty() {
        vec![adapter_kind]
    } else {
        opts.adapters.clone()
    };
    for a in &ladder {
        preflight_adapter(*a, &cfg)?;
    }

    let mut prev_signature: Option<String> = None;
    let mut total_usd_micros = 0i64;
    for iteration in 1..=iterations {
        let run_id = new_id();
        let adapter_kind = ladder[(iteration - 1).min(ladder.len() as u32 - 1) as usize];
        println!("improve iteration={iteration}/{iterations} run_id={run_id} adapter={}", adapter_kind.as_str());
        let lock = RepoLock::acquire_with(
            &repo,
            &run_id,
            &LockAcquire {
                exclusive: cfg.file.lock.repo_exclusive,
                paths: contract.allowed_globs.clone(),
                resume: false,
            },
        )?;
        let rsi = RsiCtx {
            iteration,
            parent_run_id: parent_run_id.clone(),
            notes: notes.clone(),
        };
        let result = run_factory(
            &store,
            &repo,
            contract.clone(),
            contract_abs.clone(),
            adapter_kind,
            &cfg,
            run_id.clone(),
            Some(&rsi),
        )
        .await;
        let _ = lock.release();
        // An unsuccessful *terminal* outcome (escalate, rework limit, budget,
        // failed) is an iteration result, not an abort reason — carry its
        // evidence forward. Only propagate when the run never reached a
        // terminal state (true infra failure).
        let run = match (result, store.get_run(&run_id).await) {
            (Ok(()), run) => run?,
            (Err(e), Ok(run)) if run.state.is_terminal() => {
                eprintln!("improve iteration={iteration} run_error={e}");
                run
            }
            (Err(e), _) => return Err(e),
        };
        total_usd_micros += run.spent_usd_micros;
        println!(
            "improve iteration={iteration} finished state={} spent_usd={} turns={} loop_total_usd={}",
            run.state.as_str(),
            wanax_core::money::format_usd_4(run.spent_usd_micros),
            run.spent_inner_turns,
            wanax_core::money::format_usd_4(total_usd_micros),
        );
        if let Some(cap) = opts.max_total_usd_micros {
            if total_usd_micros >= cap && run.state != RunState::Accepted {
                return Err(WanaxError::new(
                    ErrorCode::Budget,
                    format!(
                        "improve: loop budget exhausted ({} >= {})",
                        wanax_core::money::format_usd_4(total_usd_micros),
                        wanax_core::money::format_usd_4(cap),
                    ),
                ));
            }
        }
        match run.state {
            RunState::Accepted => {
                println!("improve: accepted at iteration {iteration}/{iterations}");
                return Ok(());
            }
            RunState::Cancelled | RunState::Canceling => {
                return Err(WanaxError::with_detail(
                    ErrorCode::Db,
                    "improve: run cancelled",
                ));
            }
            _ => {}
        }
        parent_run_id = Some(run_id.clone());
        let next_notes = evidence_notes(&repo, &run_id);
        let signature = notes_signature(&next_notes);
        if prev_signature.as_deref() == Some(signature.as_str()) {
            // Identical evidence two iterations in a row = zero progress.
            // Stop instead of burning the remaining iterations.
            return Err(WanaxError::new(
                ErrorCode::ReworkLimit,
                "improve: no progress between iterations, stopped early",
            ));
        }
        prev_signature = Some(signature);
        notes = Some(next_notes);
    }
    Err(WanaxError::new(
        ErrorCode::ReworkLimit,
        format!("improve: not accepted after {iterations} iterations"),
    ))
}

/// Mine a finished run's tombstone for what the next iteration should know:
/// final state, verdict sequence, outer exit codes, boundary violations,
/// error-code histogram. Mechanical concatenation — no model summarization.
fn evidence_notes(repo: &Path, run_id: &str) -> String {
    let mut parts = vec![format!("prior_run_id={run_id}")];
    if let Ok(env) = wanax_tombstone::load_envelope(repo, run_id) {
        parts.push(format!("final_state={}", env.current_state));
        let verdicts: Vec<String> = env
            .events
            .iter()
            .filter(|e| e.kind == EventKind::Verdict)
            .map(|e| {
                format!(
                    "{} ({})",
                    e.payload["decision"].as_str().unwrap_or("?"),
                    e.payload["reason"].as_str().unwrap_or("")
                )
            })
            .collect();
        if !verdicts.is_empty() {
            parts.push(format!("verdicts: {}", verdicts.join(" -> ")));
        }
        let exits: Vec<i64> = env
            .events
            .iter()
            .filter(|e| e.kind == EventKind::OuterTestFinished)
            .filter_map(|e| e.payload["exit_code"].as_i64())
            .collect();
        if !exits.is_empty() {
            parts.push(format!("outer_test_exits: {exits:?}"));
        }
        let mut errors: HashMap<String, u32> = HashMap::new();
        let mut boundary: Vec<String> = Vec::new();
        for e in env.events.iter().filter(|e| e.kind == EventKind::Error) {
            if let Some(c) = e.payload["code"].as_str() {
                *errors.entry(c.to_string()).or_default() += 1;
            }
            if let Some(paths) = e.payload["paths"].as_array() {
                for p in paths {
                    if let Some(s) = p.as_str() {
                        boundary.push(s.to_string());
                    }
                }
            }
        }
        if !boundary.is_empty() {
            parts.push(format!("boundary_violations: {}", boundary.join(", ")));
        }
        if !errors.is_empty() {
            let codes = errors
                .iter()
                .map(|(k, v)| format!("{k}x{v}"))
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!("error_codes: {codes}"));
        }
        let baseline = env
            .events
            .iter()
            .find(|e| e.kind == EventKind::BaselineTest)
            .and_then(|e| e.payload["exit_code"].as_i64());
        if let Some(b) = baseline {
            parts.push(format!("baseline_test_exit={b}"));
        }
    }
    parts.join("\n")
}

fn learnings_path(repo: &Path) -> PathBuf {
    repo.join(".wanax").join("LEARNINGS.md")
}

fn contract_label(contract: &Contract) -> String {
    contract.name.clone().unwrap_or_else(|| {
        contract.content_sha256.chars().take(8).collect()
    })
}

/// Cross-run factory memory: one distilled line per finished run, plus a
/// `## lifetime` header recomputed from ALL entries so long-lived facts
/// (per-contract accept rates, error histograms) never slide out of the
/// bounded dispatch window. `.wanax/` is gitignored — local evidence only.
fn append_learning(repo: &Path, run_id: &str, contract_label: &str) {
    let Ok(env) = wanax_tombstone::load_envelope(repo, run_id) else {
        return;
    };
    let mut errors: HashMap<String, u32> = HashMap::new();
    let mut verdicts: Vec<String> = Vec::new();
    let mut outer_exits: Vec<i64> = Vec::new();
    let mut sandbox = None;
    for e in &env.events {
        match e.kind {
            EventKind::Verdict => {
                if let Some(d) = e.payload["decision"].as_str() {
                    verdicts.push(d.to_string());
                }
            }
            EventKind::OuterTestFinished => {
                if let Some(c) = e.payload["exit_code"].as_i64() {
                    outer_exits.push(c);
                }
            }
            EventKind::Error => {
                if let Some(c) = e.payload["code"].as_str() {
                    *errors.entry(c.to_string()).or_default() += 1;
                }
            }
            EventKind::RunStarted => {
                sandbox = e.payload["sandbox"].as_str().map(String::from);
            }
            _ => {}
        }
    }
    let codes = errors
        .iter()
        .map(|(k, v)| format!("{k}x{v}"))
        .collect::<Vec<_>>()
        .join(",");
    let line = format!(
        "- {} run={} contract={} state={} verdicts=[{}] outer_exits={:?} errors={} sandbox={}",
        now_rfc3339(),
        run_id,
        contract_label,
        env.current_state,
        verdicts.join(">"),
        outer_exits,
        if codes.is_empty() { "-".into() } else { codes },
        sandbox.unwrap_or_else(|| "-".into()),
    );

    // Rebuild: entries are the source of truth, lifetime recomputed from them.
    let path = learnings_path(repo);
    let mut entries: Vec<String> = fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with("- "))
        .map(String::from)
        .collect();
    entries.push(line);

    let mut n_runs = 0u32;
    let mut n_accepted = 0u32;
    let mut err_hist: HashMap<String, u32> = HashMap::new();
    let mut by_contract: HashMap<String, (u32, u32)> = HashMap::new();
    for e in &entries {
        n_runs += 1;
        let accepted = e.contains("state=accepted");
        if accepted {
            n_accepted += 1;
        }
        if let Some(errs) = e.split_whitespace().find_map(|t| t.strip_prefix("errors=")) {
            if errs != "-" {
                for c in errs.split(',') {
                    let (code, n) = c.split_once('x').unwrap_or((c, "1"));
                    *err_hist.entry(code.to_string()).or_default() +=
                        n.parse::<u32>().unwrap_or(1);
                }
            }
        }
        if let Some(label) = e.split_whitespace().find_map(|t| t.strip_prefix("contract=")) {
            let en = by_contract.entry(label.to_string()).or_default();
            en.0 += 1;
            if accepted {
                en.1 += 1;
            }
        }
    }
    let mut text = String::from("# Wanax Learnings\n\n## lifetime\n");
    text.push_str(&format!("runs={n_runs} accepted={n_accepted}\n"));
    if !err_hist.is_empty() {
        let mut codes: Vec<String> = err_hist
            .iter()
            .map(|(k, v)| format!("{k}x{v}"))
            .collect();
        codes.sort();
        text.push_str(&format!("top_errors: {}\n", codes.join(",")));
    }
    let mut cnames: Vec<(&String, &(u32, u32))> = by_contract.iter().collect();
    cnames.sort();
    for (name, (fin, acc)) in cnames {
        text.push_str(&format!("contract {name}: {acc}/{fin}\n"));
    }
    text.push_str("\n## entries\n");
    for e in &entries {
        text.push_str(e);
        text.push('\n');
    }
    let _ = fs::write(&path, text);
}

fn learnings_tail(repo: &Path, max_lines: usize) -> Option<String> {
    let text = fs::read_to_string(learnings_path(repo)).ok()?;
    let tail: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with('-'))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .take(max_lines)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if tail.is_empty() {
        None
    } else {
        Some(tail.join("\n"))
    }
}

/// The `## lifetime` block — long-run aggregates that survive the tail window.
fn learnings_lifetime(repo: &Path) -> Option<String> {
    let text = fs::read_to_string(learnings_path(repo)).ok()?;
    let start = text.find("## lifetime")?;
    let end = text.find("## entries").unwrap_or(text.len());
    let block = text[start..end].trim().to_string();
    if block.len() <= "## lifetime".len() {
        None
    } else {
        Some(block)
    }
}

/// Merge per-iteration evidence with accumulated cross-run learnings:
/// lifetime aggregates (never decay) + the last 20 entries (recency).
fn compose_rsi_notes(rsi_notes: Option<&str>, repo: &Path) -> Option<String> {
    let lifetime = learnings_lifetime(repo);
    let tail = learnings_tail(repo, 20);
    let learnings = match (lifetime, tail) {
        (Some(lt), Some(t)) => Some(format!("{lt}\nRecent:\n{t}")),
        (Some(lt), None) => Some(lt),
        (None, t) => t,
    };
    match (rsi_notes, learnings) {
        (Some(n), Some(l)) => Some(format!(
            "{n}\n\nAccumulated learnings across runs:\n{l}"
        )),
        (Some(n), None) => Some(n.to_string()),
        (None, Some(l)) => Some(format!("Accumulated learnings across runs:\n{l}")),
        (None, None) => None,
    }
}

/// Failure-mode signature of an iteration: decisions, exit codes, boundary
/// paths, error classes — but not run ids or excerpt text (those embed
/// worktree paths and would never match). Two identical signatures in a row
/// mean zero progress; the loop should stop rather than burn budget.
fn notes_signature(notes: &str) -> String {
    let mut out = Vec::new();
    for l in notes.lines() {
        if let Some(v) = l.strip_prefix("verdicts: ") {
            let decisions = v
                .split(" -> ")
                .map(|d| d.split(" (").next().unwrap_or(d))
                .collect::<Vec<_>>()
                .join(">");
            out.push(format!("verdicts={decisions}"));
        } else if l.starts_with("final_state=")
            || l.starts_with("outer_test_exits:")
            || l.starts_with("boundary_violations:")
            || l.starts_with("error_codes:")
            || l.starts_with("baseline_test_exit=")
        {
            out.push(l.to_string());
        }
    }
    out.join("\n")
}

pub async fn resume(opts: ResumeOpts) -> Result<(), WanaxError> {
    let cwd = std::env::current_dir().map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    require_git_repo(&cwd)?;
    let repo = repo_root(&cwd)?
        .canonicalize()
        .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    fs::create_dir_all(&opts.data_dir).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    let store = Store::open(&opts.data_dir.join("wanax.db")).await?;
    let mut run = match opts.run_id {
        Some(id) => store.get_run(&id).await?,
        None => store.latest_active_run(&repo.display().to_string()).await?,
    };
    if run.state.is_terminal() {
        return Err(WanaxError::with_detail(
            ErrorCode::Resume,
            format!("run {} is {}", run.id, run.state.as_str()),
        ));
    }
    let repo_run = PathBuf::from(&run.repo_root)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&run.repo_root));
    if repo_run != repo {
        return Err(WanaxError::with_detail(
            ErrorCode::Resume,
            "run repo_root does not match current repository",
        ));
    }
    if !opts.allow_dirty {
        let dirty = dirty_non_wanax(&repo)?;
        if !dirty.is_empty() {
            return Err(WanaxError::from_code(ErrorCode::DirtyWorktree));
        }
    }
    let cfg = load_merged_config(&repo, &opts.data_dir)?;
    let adapter_kind = opts.adapter.unwrap_or(run.worker_adapter);
    preflight_adapter(adapter_kind, &cfg)?;
    let contract = store.get_contract(&run.contract_id).await?;
    let contract_abs = repo.join(&contract.path);
    let lock = RepoLock::acquire_with(
        &repo,
        &run.id,
        &LockAcquire {
            exclusive: cfg.file.lock.repo_exclusive,
            paths: contract.allowed_globs.clone(),
            resume: true,
        },
    )?;
    run.start_pid = Some(i64::from(std::process::id()));
    store.save_run_progress(&run).await?;
    println!("{}", run.id);
    println!("state={}", run.state.as_str());
    let result = continue_factory(
        &store,
        &repo,
        contract,
        contract_abs,
        adapter_kind,
        &cfg,
        run,
    )
    .await;
    let _ = lock.release();
    result
}

/// Cross-run iteration context for `wanax improve`: what failed last time.
/// The contract stays frozen across iterations — only the evidence moves.
#[derive(Debug, Clone, Default)]
pub struct RsiCtx {
    pub iteration: u32,
    pub parent_run_id: Option<String>,
    pub notes: Option<String>,
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
async fn run_factory(
    store: &Store,
    repo: &Path,
    mut contract: Contract,
    contract_abs: PathBuf,
    adapter_kind: WorkerAdapterKind,
    cfg: &ResolvedConfig,
    run_id: String,
    rsi: Option<&RsiCtx>,
) -> Result<(), WanaxError> {
    let now = now_rfc3339();
    let base_sha = head_sha(repo)?;
    contract.id = new_id();
    store.insert_contract(&contract).await?;

    let mut run = FactoryRun {
        id: run_id.clone(),
        repo_root: repo.display().to_string(),
        contract_id: contract.id.clone(),
        contract_sha256: contract.content_sha256.clone(),
        state: RunState::ContractReady,
        base_sha: base_sha.clone(),
        inner_branch: inner_branch_name(&run_id),
        outer_branch: outer_branch_name(&run_id),
        commander_model: cfg.file.commander.model.clone(),
        inner_model: cfg.file.inner.model.clone(),
        reviewer_model: cfg.file.reviewer.model.clone(),
        max_usd_micros: cfg.max_usd_micros,
        max_inner_turns: cfg.file.budget.max_inner_turns,
        spent_usd_micros: 0,
        spent_inner_turns: 0,
        worker_adapter: adapter_kind,
        created_at: now.clone(),
        updated_at: now,
        finished_at: None,
        last_error: None,
        worker_pid: None,
        start_pid: Some(i64::from(std::process::id())),
    };
    store.insert_run(&run).await?;
    println!("{}", run.id);
    println!("state={}", run.state.as_str());

    create_branch(repo, &run.inner_branch, &base_sha)?;
    create_branch(repo, &run.outer_branch, &base_sha)?;

    let inner_wt = repo
        .join(".wanax")
        .join("worktrees")
        .join(format!("{}-inner", run.id));
    worktree_add_branch(repo, &inner_wt, &run.inner_branch)?;
    harden_inner_worktree(&inner_wt)?;
    let wrapper_dir = install_git_wrapper(&inner_wt, &cfg.file.git.protected_refs)?;

    copy_fake_specs(repo, &inner_wt);

    logging::init_run_log(&run_dir(repo, &run.id).join("wanax.log")).ok();

    let gitconfig = prepare_worker_gitconfig(repo)?;
    let egress = build_egress(cfg).await?;
    let test_env = test_child_env(cfg, &gitconfig, egress.proxy_url.as_deref());
    let inner_sandbox = sandbox_for(cfg, repo, &run.id, &inner_wt, "inner", egress.net)?;

    let mut started_payload =
        json!({ "base_sha": base_sha, "sandbox": inner_sandbox.backend.as_str(), "adapter": adapter_kind.as_str(), "contract_path": contract.path, "egress": egress.label() });
    if let Some(r) = rsi {
        started_payload["iteration"] = json!(r.iteration);
        if let Some(p) = &r.parent_run_id {
            started_payload["parent_run_id"] = json!(p);
        }
    }
    let started = make_event(Actor::System, EventKind::RunStarted, started_payload);
    init_envelope(repo, &run.id, &run.contract_sha256, "dispatched", started)?;

    // Baseline: run the bound test on the untouched base so the verdict can
    // distinguish red→green from green→green. Non-fatal; the run continues.
    let mut baseline_exit = None;
    if cfg.file.verify.baseline_test {
        let r = run_test_command(
            &inner_wt,
            &contract.test_command,
            contract.test_timeout_secs,
            &test_env,
            Some(&inner_sandbox),
        )?;
        let code = if r.timed_out { 124 } else { r.exit_code };
        baseline_exit = Some(code);
        let _ = append_event(
            repo,
            &run.id,
            "dispatched",
            make_event(
                Actor::Verifier,
                EventKind::BaselineTest,
                json!({
                    "exit_code": code,
                    "cwd": r.cwd.display().to_string(),
                    "timed_out": r.timed_out,
                    "excerpt": r.excerpt,
                }),
            ),
        );
    }
    if allowed_globs_cover_binding_tests(&contract.allowed_globs) {
        let _ = append_event(
            repo,
            &run.id,
            "dispatched",
            make_event(
                Actor::System,
                EventKind::Error,
                json!({
                    "code": ErrorCode::ContractTestsWritable.as_str(),
                    "note": "WARN",
                }),
            ),
        );
    }
    store
        .set_state(&mut run, RunState::Dispatched, None)
        .await?;
    println!("state={}", run.state.as_str());

    let learn_label = contract_label(&contract);
    let result = drive_factory(DriveParams {
        store,
        repo,
        contract,
        contract_abs,
        adapter_kind,
        cfg,
        run,
        inner_wt,
        wrapper_dir,
        base_sha,
        unit: None,
        dag_units: None,
        pending_outer: None,
        outer_n: 0,
        gitconfig,
        test_env,
        inner_sandbox,
        baseline_exit,
        rsi_notes: rsi.and_then(|r| r.notes.clone()),
        net: egress.net,
        proxy_url: egress.proxy_url.clone(),
    })
    .await;
    // Persistent factory memory: one distilled line per run, so future
    // dispatches stand on accumulated evidence, not just the last failure.
    append_learning(repo, &run_id, &learn_label);
    result
}

#[allow(clippy::too_many_lines)]
async fn continue_factory(
    store: &Store,
    repo: &Path,
    contract: Contract,
    contract_abs: PathBuf,
    adapter_kind: WorkerAdapterKind,
    cfg: &ResolvedConfig,
    run: FactoryRun,
) -> Result<(), WanaxError> {
    let base_sha = run.base_sha.clone();
    if !ref_exists(repo, &run.inner_branch) {
        create_branch(repo, &run.inner_branch, &base_sha)?;
    }
    if !ref_exists(repo, &run.outer_branch) {
        create_branch(repo, &run.outer_branch, &base_sha)?;
    }
    let inner_wt = repo
        .join(".wanax")
        .join("worktrees")
        .join(format!("{}-inner", run.id));
    if !inner_wt.join(".git").exists() && !inner_wt.join(".git").is_file() {
        worktree_add_branch(repo, &inner_wt, &run.inner_branch)?;
    }
    harden_inner_worktree(&inner_wt)?;
    let wrapper_dir = install_git_wrapper(&inner_wt, &cfg.file.git.protected_refs)?;
    copy_fake_specs(repo, &inner_wt);
    logging::init_run_log(&run_dir(repo, &run.id).join("wanax.log")).ok();
    let gitconfig = prepare_worker_gitconfig(repo)?;
    let egress = build_egress(cfg).await?;
    let test_env = test_child_env(cfg, &gitconfig, egress.proxy_url.as_deref());
    let inner_sandbox = sandbox_for(cfg, repo, &run.id, &inner_wt, "inner", egress.net)?;
    let baseline_exit = baseline_exit_from_envelope(repo, &run.id);
    let _ = append_event(
        repo,
        &run.id,
        run.state.as_str(),
        make_event(
            Actor::System,
            EventKind::StateChanged,
            json!({ "phase": "resume" }),
        ),
    );

    let mut existing = store.work_units_for_run(&run.id).await?;
    let mut pending_outer = None;

    // The tombstone envelope is the source of truth: when it carries
    // replay-complete unit evidence, rebuild `existing` from the event
    // stream and heal any diverged/missing DB rows. Old envelopes lack
    // unit_seeded events — replay returns None and the DB path is kept.
    if let Some(snap) = replay::envelope_snapshot(repo, &run.id) {
        if !snap.units.is_empty() {
            if replay::units_diverged(&existing, &snap.units) {
                let _ = append_event(
                    repo,
                    &run.id,
                    run.state.as_str(),
                    make_event(
                        Actor::System,
                        EventKind::Error,
                        json!({
                            "code": ErrorCode::Resume.as_str(),
                            "note": "WARN",
                            "detail": "db work_units diverged from tombstone; rebuilt from envelope",
                        }),
                    ),
                );
                for u in &snap.units {
                    match store.get_work_unit(&u.id).await {
                        Ok(dbu)
                            if dbu.state == u.state
                                && dbu.rework_count == u.rework_count
                                && dbu.receipt_id == u.receipt_id => {}
                        Ok(_) => {
                            store.update_work_unit(u).await?;
                        }
                        Err(_) => {
                            store.insert_work_unit(u).await?;
                        }
                    }
                }
            }
            existing = snap.units.clone();
            if matches!(
                run.state,
                RunState::ReceiptReady | RunState::OuterReviewing
            ) {
                if let Some((uid, rid)) = &snap.pending {
                    if let Some(u) = snap.units.iter().find(|x| &x.id == uid) {
                        let receipt = match store.get_receipt(rid).await {
                            Ok(r) => r,
                            Err(_) => {
                                let stub = &snap.receipts[uid];
                                replay::synthesize_receipt(
                                    stub,
                                    &contract.test_command,
                                    adapter_kind.as_str(),
                                )
                            }
                        };
                        pending_outer = Some((u.clone(), receipt));
                    }
                }
            }
        }
    }

    let dag_units = existing
        .iter()
        .any(|u| u.local_key.is_some() || !u.depends_on.is_empty())
        .then(|| existing.clone());
    let unit = existing.last().cloned();
    if pending_outer.is_none()
        && matches!(
            run.state,
            RunState::ReceiptReady | RunState::OuterReviewing
        )
    {
        if let Some(u) = unit.clone() {
            if let Some(rid) = &u.receipt_id {
                if let Ok(receipt) = store.get_receipt(rid).await {
                    pending_outer = Some((u, receipt));
                }
            }
        }
    }

    // A crash between `contract_ready` and `dispatched` (e.g. during the
    // baseline test) resumes here: replay the missing transition.
    let mut run = run;
    if run.state == RunState::ContractReady {
        store.set_state(&mut run, RunState::Dispatched, None).await?;
    }
    let run_id = run.id.clone();

    let learn_label = contract_label(&contract);
    let result = drive_factory(DriveParams {
        store,
        repo,
        contract,
        contract_abs,
        adapter_kind,
        cfg,
        run,
        inner_wt,
        wrapper_dir,
        base_sha,
        unit,
        dag_units,
        pending_outer,
        outer_n: 0,
        gitconfig,
        test_env,
        inner_sandbox,
        baseline_exit,
        rsi_notes: None,
        net: egress.net,
        proxy_url: egress.proxy_url.clone(),
    })
    .await;
    append_learning(repo, &run_id, &learn_label);
    result
}

struct DriveParams<'a> {
    store: &'a Store,
    repo: &'a Path,
    contract: Contract,
    contract_abs: PathBuf,
    adapter_kind: WorkerAdapterKind,
    cfg: &'a ResolvedConfig,
    run: FactoryRun,
    inner_wt: PathBuf,
    wrapper_dir: PathBuf,
    base_sha: String,
    unit: Option<WorkUnit>,
    dag_units: Option<Vec<WorkUnit>>,
    pending_outer: Option<(WorkUnit, Receipt)>,
    outer_n: u32,
    gitconfig: PathBuf,
    test_env: HashMap<String, String>,
    inner_sandbox: SandboxSpec,
    baseline_exit: Option<i32>,
    rsi_notes: Option<String>,
    net: wanax_core::sandbox::NetConfine,
    proxy_url: Option<String>,
}

#[allow(clippy::too_many_lines)]
async fn drive_factory(params: DriveParams<'_>) -> Result<(), WanaxError> {
    let DriveParams {
        store,
        repo,
        contract,
        contract_abs,
        adapter_kind,
        cfg,
        mut run,
        inner_wt,
        wrapper_dir,
        base_sha,
        mut unit,
        mut dag_units,
        mut pending_outer,
        mut outer_n,
        gitconfig,
        test_env,
        inner_sandbox,
        baseline_exit,
        rsi_notes,
        net,
        proxy_url,
    } = params;
    let commander: Box<dyn Commander> = pick_commander(cfg)?;
    let review_client = pick_review_client(cfg);
    let mut rework_notes: Option<String> = None;
    let mut reuse_existing = unit.is_some() && pending_outer.is_none() && dag_units.is_none();
    // Wave rework bookkeeping persists across loop turns: each retry creates a
    // fresh master unit, so its own rework_count would never advance.
    let mut wave_rework: u32 = 0;
    let mut wave_set_key = String::new();

    let outcome = loop {
        if is_budget_exhausted(&run) {
            break fail_budget(store, repo, &mut run).await;
        }

        // Dag-unit ids covered by a parallel wave this turn — marked accepted
        // together when the merged wave passes the outer review. wave_pre_sha
        // lets a reworked wave roll the inner tree back before re-merging.
        let mut wave_ids: Vec<String> = Vec::new();
        let mut wave_pre_sha: Option<String> = None;
        let mut bound_failing: Vec<String> = Vec::new();
        let mut receipt_already_stored = false;
        let inner_result = if let Some((u0, r0)) = pending_outer.take() {
            receipt_already_stored = true;
            Ok(InnerPhaseResult {
                unit: u0,
                receipt: r0,
            })
        } else if let Some(ref dags) = dag_units {
            // Wave parallelism: ready units with declared, mutually disjoint
            // allowed_globs run as ONE peer batch (separate worktrees merged
            // into inner), reviewed by a single outer pass. The PRD's
            // "disjoint modules parallel" applied to the DAG, not just peers.
            let ready: Vec<WorkUnit> = dags
                .iter()
                .filter(|u| dag_ready(u, dags))
                .cloned()
                .collect();
            let wave = dag_wave(&ready);
            if wave.len() >= 2 {
                wave_ids = wave.iter().map(|u| u.id.clone()).collect();
                wave_pre_sha = Some(head_sha(&inner_wt)?);
                let mut key_parts = wave_ids.clone();
                key_parts.sort();
                let key = key_parts.join(",");
                if key != wave_set_key {
                    wave_rework = 0;
                    wave_set_key = key;
                }
                let peers: Vec<PeerUnitDraft> = wave
                    .iter()
                    .map(|u| PeerUnitDraft {
                        title: u.title.clone(),
                        instruction: u.instruction.clone(),
                        allowed_globs: u.allowed_globs.clone().unwrap_or_default(),
                    })
                    .collect();
                run_peer_batch(PeerBatchParams {
                    store,
                    repo,
                    run: &mut run,
                    contract: &contract,
                    cfg,
                    inner_wt: &inner_wt,
                    wrapper_dir: &wrapper_dir,
                    adapter_kind,
                    base_sha: &base_sha,
                    peers,
                    covers: wave_ids.clone(),
                    gitconfig: &gitconfig,
                    test_env: &test_env,
                    inner_sandbox: &inner_sandbox,
                proxy_url: proxy_url.as_deref(),
                net,
                })
                .await
            } else {
            match next_ready_dag(dags) {
                Some(mut next) => {
                    let draft = WorkUnitDraft {
                        title: next.title.clone(),
                        instruction: next.instruction.clone(),
                    };
                    run_single_unit(InnerSingleParams {
                        store,
                        repo,
                        run: &mut run,
                        contract: &contract,
                        cfg,
                        inner_wt: &inner_wt,
                        wrapper_dir: &wrapper_dir,
                        adapter_kind,
                        base_sha: &base_sha,
                        unit: Some(&mut next),
                        draft,
                        review_client: review_client.as_deref(),
                        gitconfig: &gitconfig,
                        test_env: &test_env,
                        inner_sandbox: &inner_sandbox,
                    proxy_url: proxy_url.as_deref(),
                    })
                    .await
                }
                None if dags.iter().all(|u| u.state == WorkUnitState::Accepted) => {
                    store.set_state(&mut run, RunState::Accepted, None).await?;
                    println!("state={}", run.state.as_str());
                    break Ok(());
                }
                None => {
                    break fail_run(
                        store,
                        repo,
                        &mut run,
                        WanaxError::from_code(ErrorCode::DagCycle),
                    )
                    .await;
                }
            }
            }
        } else if reuse_existing {
            reuse_existing = false;
            let mut existing = unit.clone().ok_or_else(|| {
                WanaxError::from_code(ErrorCode::Resume)
            })?;
            let draft = WorkUnitDraft {
                title: existing.title.clone(),
                instruction: existing.instruction.clone(),
            };
            run_single_unit(InnerSingleParams {
                store,
                repo,
                run: &mut run,
                contract: &contract,
                cfg,
                inner_wt: &inner_wt,
                wrapper_dir: &wrapper_dir,
                adapter_kind,
                base_sha: &base_sha,
                unit: Some(&mut existing),
                draft,
                review_client: review_client.as_deref(),
                gitconfig: &gitconfig,
                test_env: &test_env,
                inner_sandbox: &inner_sandbox,
                proxy_url: proxy_url.as_deref(),
            })
            .await
        } else {
            let (plan, draft_usage) = match dispatch_with_retry(
                commander.as_ref(),
                &DispatchContext {
                    contract: contract.clone(),
                    rework_notes: rework_notes.clone(),
                    rsi_notes: compose_rsi_notes(rsi_notes.as_deref(), repo),
                },
            )
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    break fail_run(store, repo, &mut run, e).await;
                }
            };
            charge_and_tick(
                store,
                repo,
                &mut run,
                &draft_usage,
                cfg.commander_in_micros,
                cfg.commander_out_micros,
            )
            .await?;
            if is_budget_exhausted(&run) {
                break fail_budget(store, repo, &mut run).await;
            }
            match plan {
                DispatchPlan::Peers(_peers) if unit.is_some() => {
                    break fail_run(
                        store,
                        repo,
                        &mut run,
                        WanaxError::new(
                            ErrorCode::CommanderSchema,
                            "peer batch rework not supported",
                        ),
                    )
                    .await;
                }
                DispatchPlan::Peers(peers) => {
                    run_peer_batch(PeerBatchParams {
                        store,
                        repo,
                        run: &mut run,
                        contract: &contract,
                        cfg,
                        inner_wt: &inner_wt,
                        wrapper_dir: &wrapper_dir,
                        adapter_kind,
                        base_sha: &base_sha,
                        peers,
                        covers: Vec::new(),
                        gitconfig: &gitconfig,
                        test_env: &test_env,
                        inner_sandbox: &inner_sandbox,
                    proxy_url: proxy_url.as_deref(),
                    net,
                    })
                    .await
                }
                DispatchPlan::Dag(drafts) => {
                    match seed_dag_units(store, repo, &run.id, drafts).await {
                        Ok(seeded) => {
                            dag_units = Some(seeded);
                            continue;
                        }
                        Err(e) => break fail_run(store, repo, &mut run, e).await,
                    }
                }
                DispatchPlan::Single(draft) => {
                    run_single_unit(InnerSingleParams {
                        store,
                        repo,
                        run: &mut run,
                        contract: &contract,
                        cfg,
                        inner_wt: &inner_wt,
                        wrapper_dir: &wrapper_dir,
                        adapter_kind,
                        base_sha: &base_sha,
                        unit: unit.as_mut(),
                        draft,
                        review_client: review_client.as_deref(),
                        gitconfig: &gitconfig,
                        test_env: &test_env,
                        inner_sandbox: &inner_sandbox,
                    proxy_url: proxy_url.as_deref(),
                    })
                    .await
                }
            }
        };
        let inner_result = match inner_result {
            Ok(v) => v,
            Err(e) => break fail_run(store, repo, &mut run, e).await,
        };
        unit = Some(inner_result.unit.clone());
        let u = inner_result.unit;
        let mut receipt = inner_result.receipt;
        if let Some(cmd) = &u.test_command {
            receipt.test_command = cmd.clone();
        }
        if !receipt_already_stored {
            store.insert_receipt(&receipt).await?;
            if let Some(existing) = unit.as_mut() {
                existing.receipt_id = Some(receipt.id.clone());
                existing.inner_commit_sha = Some(receipt.commit_sha.clone());
                existing.state = WorkUnitState::ReceiptReady;
                store.update_work_unit(existing).await?;
            }
            let _ = append_event(
                repo,
                &run.id,
                "receipt_ready",
                make_event(
                    Actor::Master,
                    EventKind::ReceiptSubmitted,
                    json!({
                        "receipt_id": receipt.id,
                        "work_unit_id": u.id,
                        "claimed_pass": receipt.claimed_pass,
                        "test_exit_code": receipt.test_exit_code,
                        "changed_files": receipt.changed_files,
                        "commit_sha": receipt.commit_sha,
                    }),
                ),
            );
            store
                .set_state(&mut run, RunState::ReceiptReady, None)
                .await?;
            println!("state={}", run.state.as_str());
        }

        if run.spent_inner_turns >= run.max_inner_turns && receipt.changed_files.is_empty() {
            break fail_budget(store, repo, &mut run).await;
        }

        store
            .set_state(&mut run, RunState::OuterReviewing, None)
            .await?;
        println!("state={}", run.state.as_str());

        outer_n += 1;
        let outer_wt = repo
            .join(".wanax")
            .join("worktrees")
            .join(format!("{}-outer-{outer_n}", run.id));
        worktree_add_detach(repo, &outer_wt, &receipt.commit_sha)?;
        if outer_wt == inner_wt {
            break fail_run(
                store,
                repo,
                &mut run,
                WanaxError::new(ErrorCode::Db, "outer worktree reused inner cwd"),
            )
            .await;
        }
        let _ = append_event(
            repo,
            &run.id,
            "outer_reviewing",
            make_event(
                Actor::Verifier,
                EventKind::OuterTestStarted,
                json!({
                    "cwd": outer_wt.display().to_string(),
                    "inner_cwd": inner_wt.display().to_string(),
                    "commit_sha": receipt.commit_sha,
                }),
            ),
        );

        let test_cmd = u
            .test_command
            .as_deref()
            .unwrap_or(contract.test_command.as_str());
        let outer_spec = sandbox_for(
            cfg,
            repo,
            &run.id,
            &outer_wt,
            &format!("outer-{outer_n}"),
            net,
        )?;
        let test = run_test_command(
            &outer_wt,
            test_cmd,
            contract.test_timeout_secs,
            &test_env,
            Some(&outer_spec),
        )?;
        let outer_code = if test.timed_out { 124 } else { test.exit_code };
        let _ = append_event(
            repo,
            &run.id,
            "outer_reviewing",
            make_event(
                Actor::Verifier,
                EventKind::OuterTestFinished,
                json!({
                    "exit_code": outer_code,
                    "cwd": test.cwd.display().to_string(),
                    "timed_out": test.timed_out,
                }),
            ),
        );

        let changed = diff_name_only(repo, &base_sha, &receipt.commit_sha)?;
        let boundary =
            check_boundaries(&changed, &contract.allowed_globs, &contract.forbidden_globs)?;
        if !boundary.ok {
            let _ = append_event(
                repo,
                &run.id,
                "outer_reviewing",
                make_event(
                    Actor::Verifier,
                    EventKind::Error,
                    json!({
                        "code": ErrorCode::Boundary.as_str(),
                        "paths": boundary.violating,
                    }),
                ),
            );
        }

        // Content-level secret scan on the diff's added lines — forbidden
        // globs only see paths; a key committed into src/** is still a leak.
        let secrets = wanax_git::diff_patch(repo, &base_sha, &receipt.commit_sha)
            .map(|d| wanax_verify::scan_diff_added(&d))
            .unwrap_or_default();
        if !secrets.is_empty() {
            let _ = append_event(
                repo,
                &run.id,
                "outer_reviewing",
                make_event(
                    Actor::Verifier,
                    EventKind::Error,
                    json!({
                        "code": ErrorCode::SecretLeak.as_str(),
                        "findings": secrets.iter().map(|f| json!({
                            "path": f.path, "line": f.line, "kind": f.kind
                        })).collect::<Vec<_>>(),
                    }),
                ),
            );
        }

        // bound_test enforcement: a criterion may name a test that must exist
        // (verified via `cargo test -- --list` so a filter that matches
        // nothing cannot pass vacuously) and pass in the outer worktree.
        // Completion criteria become mechanical, not prompt-checked.
        let mut bound_ok = true;
        let cargo_runner = contract.test_command.starts_with("cargo test");
        for (crit_id, name) in contract
            .completion_criteria
            .iter()
            .filter_map(|c| c.bound_test.as_deref().map(|t| (c.id.as_str(), t)))
            .take(8)
        {
            let mut listed: Option<bool> = None;
            if cargo_runner {
                let list = run_test_command(
                    &outer_wt,
                    &format!("{} -- --list", test_cmd),
                    contract.test_timeout_secs,
                    &test_env,
                    Some(&outer_spec),
                )?;
                listed = Some(list.excerpt.lines().any(|l| {
                    let t = l.trim();
                    t.strip_suffix(": test").is_some_and(|s| {
                        let tn = s.trim();
                        tn == name || tn.ends_with(&format!("::{name}"))
                    })
                }));
            }
            let run_cmd = if cargo_runner {
                format!("{} {} -- --exact", test_cmd, name)
            } else {
                format!("{} {}", test_cmd, name)
            };
            let t = run_test_command(
                &outer_wt,
                &run_cmd,
                contract.test_timeout_secs,
                &test_env,
                Some(&outer_spec),
            )?;
            let ok = listed.unwrap_or(true) && t.exit_code == 0 && !t.timed_out;
            if !ok {
                bound_ok = false;
                bound_failing.push(name.to_string());
            }
            let _ = append_event(
                repo,
                &run.id,
                "outer_reviewing",
                make_event(
                    Actor::Verifier,
                    EventKind::BoundTest,
                    json!({
                        "criterion": crit_id,
                        "name": name,
                        "listed": listed,
                        "exit_code": t.exit_code,
                        "timed_out": t.timed_out,
                        "ok": ok,
                    }),
                ),
            );
        }

        let plugin = match run_verifier_plugins(cfg, &contract, &outer_wt, repo) {
            Ok(p) => p,
            Err(e) => break fail_run(store, repo, &mut run, e).await,
        };
        let _ = append_event(
            repo,
            &run.id,
            "outer_reviewing",
            make_event(
                Actor::Verifier,
                EventKind::StateChanged,
                json!({
                    "plugin": plugin.name,
                    "plugin_ok": plugin.ok,
                    "plugin_skipped": plugin.skipped,
                    "plugin_ran": plugin.ran,
                }),
            ),
        );
        if !plugin.ok && plugin.ran {
            let _ = append_event(
                repo,
                &run.id,
                "outer_reviewing",
                make_event(
                    Actor::Verifier,
                    EventKind::Error,
                    json!({
                        "code": ErrorCode::Plugin.as_str(),
                        "excerpt": plugin.excerpt,
                    }),
                ),
            );
        }

        let descendant = is_ancestor(repo, &base_sha, &receipt.commit_sha)?;
        let budget_done = is_budget_exhausted(&run);
        let current_rework = unit
            .as_ref()
            .map(|u| u.rework_count)
            .unwrap_or(0)
            .max(wave_rework);
        let gates = AcceptGates::from_parts(
            &receipt,
            test_cmd,
            outer_code,
            boundary.ok,
            descendant,
            budget_done,
            current_rework,
        )
        .with_plugin(plugin.ok)
        .with_bound_tests(bound_ok)
        .with_secrets(secrets.is_empty());

        let vctx = VerdictContext {
            contract: contract.clone(),
            receipt: receipt.clone(),
            diffstat: receipt.diffstat.clone(),
            changed_files: changed.clone(),
            outer_test_exit_code: outer_code,
            outer_test_excerpt: test.excerpt.clone(),
            boundary_ok: boundary.ok,
            rework_count: current_rework,
            baseline_test_exit_code: baseline_exit,
        };
        let (proposed, usage) = if gates.can_accept() {
            let mut last = WanaxError::from_code(ErrorCode::CommanderSchema);
            let mut got = None;
            for _ in 0..3 {
                match commander.verdict(&vctx).await {
                    Ok(v) => {
                        got = Some(v);
                        break;
                    }
                    Err(e) if e.code == ErrorCode::CommanderSchema => last = e,
                    Err(e) => {
                        last = e;
                        break;
                    }
                }
            }
            match got {
                Some(v) => v,
                None => break fail_run(store, repo, &mut run, last).await,
            }
        } else {
            match MechanicalCommander::new(run.commander_model.clone())
                .verdict(&vctx)
                .await
            {
                Ok(v) => v,
                Err(e) => break fail_run(store, repo, &mut run, e).await,
            }
        };
        charge_and_tick(
            store,
            repo,
            &mut run,
            &usage,
            cfg.commander_in_micros,
            cfg.commander_out_micros,
        )
        .await?;

        let (decision, note) = enforce_decision(proposed.decision, &gates);
        if note == Some(ErrorCode::AcceptOverride) {
            let _ = append_event(
                repo,
                &run.id,
                "outer_reviewing",
                make_event(
                    Actor::System,
                    EventKind::Error,
                    json!({
                        "code": ErrorCode::AcceptOverride.as_str(),
                        "note": "E_ACCEPT_OVERRIDE",
                    }),
                ),
            );
        }
        if note == Some(ErrorCode::Budget)
            || (decision == VerdictDecision::Accept && is_budget_exhausted(&run))
        {
            break fail_budget(store, repo, &mut run).await;
        }

        let mut reason = proposed.reason.clone();
        if note == Some(ErrorCode::AcceptOverride) {
            reason.push_str("\nE_ACCEPT_OVERRIDE");
        }
        if note == Some(ErrorCode::Plugin) {
            reason.push_str("\nE_PLUGIN");
        }
        let verdict = Verdict {
            id: new_id(),
            work_unit_id: u.id.clone(),
            decision,
            reason: reason.clone(),
            outer_test_exit_code: outer_code,
            outer_test_excerpt: test.excerpt.clone(),
            boundary_ok: boundary.ok,
            files_reviewed: proposed.files_reviewed.clone(),
            commander_model: run.commander_model.clone(),
            created_at: now_rfc3339(),
        };
        store.insert_verdict(&verdict).await?;
        if let Some(existing) = unit.as_mut() {
            existing.verdict_id = Some(verdict.id.clone());
            store.update_work_unit(existing).await?;
        }
        let _ = append_event(
            repo,
            &run.id,
            match decision {
                VerdictDecision::Accept => "accepted",
                VerdictDecision::Reject => "rejected",
                VerdictDecision::Rework => "rework",
                VerdictDecision::Escalate => "escalate",
            },
            make_event(
                Actor::Commander,
                EventKind::Verdict,
                json!({
                    "decision": decision.as_str(),
                    "reason": reason,
                    "work_unit_id": u.id,
                    "outer_test_exit_code": outer_code,
                    "boundary_ok": boundary.ok,
                    // Lets fitness tooling separate real red→green fixes from
                    // vacuous accepts on an already-green baseline.
                    "baseline_exit": baseline_exit,
                }),
            ),
        );

        match decision {
            VerdictDecision::Accept => {
                if !gates.can_accept() {
                    break fail_run(
                        store,
                        repo,
                        &mut run,
                        WanaxError::new(ErrorCode::Db, "accept gates failed after enforce"),
                    )
                    .await;
                }
                if let Some(existing) = unit.as_mut() {
                    existing.state = WorkUnitState::Accepted;
                    store.update_work_unit(existing).await?;
                }
                if let Some(ref mut dags) = dag_units {
                    if let Some(cur) = unit.as_ref() {
                        for d in dags.iter_mut() {
                            if d.id == cur.id {
                                *d = cur.clone();
                            }
                        }
                    }
                    // A parallel wave's members share the merged verdict.
                    if !wave_ids.is_empty() {
                        for d in dags.iter_mut() {
                            if wave_ids.contains(&d.id) {
                                d.state = WorkUnitState::Accepted;
                                d.receipt_id = Some(receipt.id.clone());
                                d.inner_commit_sha = Some(receipt.commit_sha.clone());
                                store.update_work_unit(d).await?;
                            }
                        }
                        wave_rework = 0;
                    }
                    if dags.iter().any(|d| d.state != WorkUnitState::Accepted) {
                        store
                            .set_state(&mut run, RunState::Dispatched, None)
                            .await?;
                        println!("state={}", run.state.as_str());
                        continue;
                    }
                }
                store.set_state(&mut run, RunState::Accepted, None).await?;
                println!("state={}", run.state.as_str());
                write_result(
                    repo,
                    &run.id,
                    "accept",
                    &receipt.commit_sha,
                    &test.excerpt,
                    run.spent_usd_micros,
                )?;
                println!("{}", run.inner_branch);
                if let Ok(stat) = diff_stat(repo, &base_sha, &receipt.commit_sha) {
                    println!("{stat}");
                }
                if let Some(url) =
                    maybe_create_github_pr(repo, &run.inner_branch, &run.id, cfg)?
                {
                    let _ = append_event(
                        repo,
                        &run.id,
                        "accepted",
                        make_event(
                            Actor::Commander,
                            EventKind::StateChanged,
                            json!({ "github_pr": url }),
                        ),
                    );
                    println!("pr={url}");
                }
                break Ok(());
            }
            VerdictDecision::Reject => {
                store.set_state(&mut run, RunState::Rejected, None).await?;
                println!("state={}", run.state.as_str());
                write_result(
                    repo,
                    &run.id,
                    "reject",
                    &receipt.commit_sha,
                    &test.excerpt,
                    run.spent_usd_micros,
                )?;
                break Err(WanaxError::new(
                    if boundary.ok {
                        ErrorCode::Rejected
                    } else {
                        ErrorCode::Boundary
                    },
                    if boundary.ok {
                        reason
                    } else {
                        format!("boundary check failed: {}", boundary.violating.join(", "))
                    },
                ));
            }
            VerdictDecision::Escalate => {
                store
                    .set_state(&mut run, RunState::Escalate, Some(reason.clone()))
                    .await?;
                println!("state={}", run.state.as_str());
                break Err(WanaxError::new(ErrorCode::Escalated, reason));
            }
            VerdictDecision::Rework => {
                // A reworked wave must not re-merge onto its own leftovers —
                // roll the inner tree back to before the wave merge. The
                // rework is charged to the DAG members and the wave counter
                // (the master unit is recreated each attempt, so its own
                // rework_count would never advance).
                if !wave_ids.is_empty() {
                    if let Some(sha) = wave_pre_sha.take() {
                        wanax_git::reset_hard(&inner_wt, &sha)?;
                    }
                    wave_rework += 1;
                    if wave_rework > MAX_REWORK {
                        store
                            .set_state(
                                &mut run,
                                RunState::Escalate,
                                Some(ErrorCode::ReworkLimit.default_message().into()),
                            )
                            .await?;
                        println!("state=escalate");
                        break Err(WanaxError::from_code(ErrorCode::ReworkLimit));
                    }
                    if let Some(ref mut dags) = dag_units {
                        for d in dags.iter_mut() {
                            if wave_ids.contains(&d.id) {
                                d.rework_count += 1;
                                store.update_work_unit(d).await?;
                            }
                        }
                    }
                    wave_ids.clear();
                }
                if let Some(existing) = unit.as_mut() {
                    existing.rework_count += 1;
                    if existing.rework_count > MAX_REWORK {
                        store
                            .set_state(
                                &mut run,
                                RunState::Escalate,
                                Some(ErrorCode::ReworkLimit.default_message().into()),
                            )
                            .await?;
                        println!("state=escalate");
                        break Err(WanaxError::from_code(ErrorCode::ReworkLimit));
                    }
                    existing.state = WorkUnitState::Queued;
                    store.update_work_unit(existing).await?;
                }
                rework_notes = Some(format!(
                    "Previous attempt failed.\nexit={outer_code}\n{}{}{}",
                    test.excerpt,
                    if bound_failing.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "bound tests missing or failing: {}\n",
                            bound_failing.join(", ")
                        )
                    },
                    if secrets.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "secret material in diff — remove it: {}\n",
                            secrets
                                .iter()
                                .map(|f| format!("{}:{} ({})", f.path, f.line, f.kind))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                ));
                store.set_state(&mut run, RunState::Rework, None).await?;
                println!("state={}", run.state.as_str());
                store
                    .set_state(&mut run, RunState::Dispatched, None)
                    .await?;
                println!("state={}", run.state.as_str());
            }
        }

        if let Ok(disk) = sha256_file(&contract_abs) {
            if disk != contract.content_sha256 {
                let _ = append_event(
                    repo,
                    &run.id,
                    run.state.as_str(),
                    make_event(
                        Actor::System,
                        EventKind::Error,
                        json!({
                            "code": ErrorCode::ContractMutated.as_str(),
                            "note": "WARN",
                        }),
                    ),
                );
            }
        }
    };

    if let Ok(mut env) = wanax_tombstone::load_envelope(repo, &run.id) {
        env.current_state = run.state.as_str().to_string();
        let _ = persist_envelope(repo, &env);
    }
    outcome
}

struct InnerPhaseResult {
    unit: WorkUnit,
    receipt: Receipt,
}

struct InnerSingleParams<'a> {
    store: &'a Store,
    repo: &'a Path,
    run: &'a mut FactoryRun,
    contract: &'a Contract,
    cfg: &'a ResolvedConfig,
    inner_wt: &'a Path,
    wrapper_dir: &'a Path,
    adapter_kind: WorkerAdapterKind,
    base_sha: &'a str,
    unit: Option<&'a mut WorkUnit>,
    draft: wanax_llm::WorkUnitDraft,
    review_client: Option<&'a dyn wanax_llm::CompletionClient>,
    gitconfig: &'a Path,
    test_env: &'a HashMap<String, String>,
    inner_sandbox: &'a SandboxSpec,
    proxy_url: Option<&'a str>,
}

struct PeerBatchParams<'a> {
    store: &'a Store,
    repo: &'a Path,
    run: &'a mut FactoryRun,
    contract: &'a Contract,
    cfg: &'a ResolvedConfig,
    inner_wt: &'a Path,
    wrapper_dir: &'a Path,
    adapter_kind: WorkerAdapterKind,
    base_sha: &'a str,
    peers: Vec<PeerUnitDraft>,
    /// Dag unit ids this wave covers (empty for explicit peer batches) —
    /// recorded in unit_dispatched so replay can mark the covered units.
    covers: Vec<String>,
    gitconfig: &'a Path,
    test_env: &'a HashMap<String, String>,
    inner_sandbox: &'a SandboxSpec,
    proxy_url: Option<&'a str>,
    net: wanax_core::sandbox::NetConfine,
}

async fn run_single_unit(params: InnerSingleParams<'_>) -> Result<InnerPhaseResult, WanaxError> {
    let InnerSingleParams {
        store,
        repo,
        run,
        contract,
        cfg,
        inner_wt,
        wrapper_dir,
        adapter_kind,
        base_sha,
        unit,
        draft,
        review_client,
        gitconfig,
        test_env,
        inner_sandbox,
        proxy_url,
    } = params;
    let u = match unit {
        Some(existing) => {
            existing.instruction = draft.instruction;
            existing.title = draft.title;
            existing.assignee_role = AssigneeRole::Goal;
            existing.state = WorkUnitState::Implementing;
            store.update_work_unit(existing).await?;
            existing.clone()
        }
        None => {
            let created = WorkUnit {
                id: new_id(),
                run_id: run.id.clone(),
                seq: 1,
                title: draft.title,
                instruction: draft.instruction,
                state: WorkUnitState::Assigned,
                assignee_role: AssigneeRole::Goal,
                parent_id: None,
                allowed_globs: None,
                depends_on: Vec::new(),
                test_command: None,
                local_key: None,
                rework_count: 0,
                inner_commit_sha: None,
                receipt_id: None,
                verdict_id: None,
            };
            store.insert_work_unit(&created).await?;
            created
        }
    };

    let _ = append_event(
        repo,
        &run.id,
        "inner_working",
        make_event(
            Actor::Commander,
            EventKind::UnitDispatched,
            // Envelope is the source of truth — every field needed to replay
            // this unit after a crash lives here.
            json!({
                "work_unit_id": u.id,
                "title": u.title,
                "instruction": u.instruction,
                "seq": u.seq,
                "role": u.assignee_role.as_str(),
                "local_key": u.local_key,
                "depends_on": u.depends_on,
                "allowed_globs": u.allowed_globs,
                "test_command": u.test_command,
            }),
        ),
    );
    store
        .set_state(run, RunState::InnerWorking, None)
        .await?;
    println!("state={}", run.state.as_str());

    fs::write(inner_wt.join("WORK_UNIT.md"), &u.instruction)
        .map_err(|e| WanaxError::with_detail(ErrorCode::WorkerCrash, e))?;

    run.worker_pid = Some(i64::from(std::process::id()));
    store.save_run_progress(run).await?;
    println!("starting worker pid={}", std::process::id());

    let unit_spec = inner_wt
        .join(".wanax")
        .join(format!("fake-unit-{}.toml", u.seq));
    let spec_path = if unit_spec.is_file() {
        unit_spec
    } else {
        inner_wt.join(".wanax").join("fake.toml")
    };
    let handle = run_goal_loop(GoalLoopParams {
        store,
        repo,
        run,
        contract,
        cfg,
        inner_wt,
        adapter_kind,
        spec_path: &spec_path,
        wrapper_dir,
        unit: &u,
        review_client,
        gitconfig,
        test_env,
        inner_sandbox,
        proxy_url,
    })
    .await?;

    if run.spent_inner_turns >= run.max_inner_turns && handle.crashed {
        return Err(budget_error(run));
    }

    let receipt = collect_inner_receipt(
        repo,
        inner_wt,
        contract,
        &u,
        adapter_kind.as_str(),
        &handle,
        base_sha,
    )?;
    Ok(InnerPhaseResult { unit: u, receipt })
}

async fn run_peer_batch(params: PeerBatchParams<'_>) -> Result<InnerPhaseResult, WanaxError> {
    let PeerBatchParams {
        store,
        repo,
        run,
        contract,
        cfg,
        inner_wt,
        wrapper_dir,
        adapter_kind,
        base_sha,
        peers,
        covers,
        gitconfig,
        test_env,
        inner_sandbox,
        proxy_url,
        net,
    } = params;
    if peers.len() < 2 {
        return Err(WanaxError::from_code(ErrorCode::CommanderSchema));
    }
    let glob_sets: Vec<Vec<String>> = peers.iter().map(|p| p.allowed_globs.clone()).collect();
    if let Some((i, j)) = find_peer_overlap(&glob_sets) {
        return Err(WanaxError::with_detail(
            ErrorCode::PeerOverlap,
            format!("peer {i} and peer {j} file sets overlap"),
        ));
    }
    for (idx, peer) in peers.iter().enumerate() {
        for path in peer_probe_paths(&peer.allowed_globs) {
            let contract_ok = check_boundaries(
                std::slice::from_ref(&path),
                &contract.allowed_globs,
                &contract.forbidden_globs,
            )?;
            let peer_ok = check_boundaries(
                std::slice::from_ref(&path),
                &peer.allowed_globs,
                &contract.forbidden_globs,
            )?;
            if peer_ok.ok && !contract_ok.ok {
                return Err(WanaxError::with_detail(
                    ErrorCode::CommanderSchema,
                    format!("peer {idx} allowed_globs exceed contract"),
                ));
            }
        }
    }

    let master = WorkUnit {
        id: new_id(),
        run_id: run.id.clone(),
        seq: 1,
        title: format!("peer-batch ({})", peers.len()),
        instruction: peers
            .iter()
            .map(|p| format!("- {}: {}", p.title, p.instruction))
            .collect::<Vec<_>>()
            .join("\n"),
        state: WorkUnitState::Assigned,
        assignee_role: AssigneeRole::Master,
        parent_id: None,
        allowed_globs: None,
        depends_on: Vec::new(),
        test_command: None,
        local_key: None,
        rework_count: 0,
        inner_commit_sha: None,
        receipt_id: None,
        verdict_id: None,
    };
    store.insert_work_unit(&master).await?;

    let _ = append_event(
        repo,
        &run.id,
        "inner_working",
        make_event(
            Actor::Commander,
            EventKind::UnitDispatched,
            json!({
                "work_unit_id": master.id,
                "title": master.title,
                "instruction": master.instruction,
                "mode": "peer_batch",
                "peer_count": peers.len(),
                "covers": covers,
            }),
        ),
    );
    store
        .set_state(run, RunState::InnerWorking, None)
        .await?;
    println!("state={}", run.state.as_str());

    run.worker_pid = Some(i64::from(std::process::id()));
    store.save_run_progress(run).await?;

    // Phase 1 (serial): per-peer branch, worktree, work unit, worker context.
    struct PeerRun {
        unit: WorkUnit,
        worktree: PathBuf,
        allowed_globs: Vec<String>,
    }
    let mut pending: Vec<(PeerRun, WorkerContext, PathBuf)> = Vec::new();
    for (idx, peer) in peers.iter().enumerate() {
        let seq = u32::try_from(idx + 1).unwrap_or(1);
        let branch = peer_branch_name(&run.id, seq);
        // Retry-safe: a rework pass re-runs the batch over branches/worktrees
        // left by the previous attempt — reset them instead of colliding.
        let peer_wt = repo
            .join(".wanax")
            .join("worktrees")
            .join(format!("{}-peer-{seq}", run.id));
        if peer_wt.exists() {
            wanax_git::worktree_remove_force(repo, &peer_wt)?;
        }
        if ref_exists(repo, &branch) {
            reset_branch(repo, &branch, base_sha)?;
        } else {
            create_branch(repo, &branch, base_sha)?;
        }
        worktree_add_branch(repo, &peer_wt, &branch)?;
        harden_inner_worktree(&peer_wt)?;
        let _ = install_git_wrapper(&peer_wt, &cfg.file.git.protected_refs)?;

        let peer_unit = WorkUnit {
            id: new_id(),
            run_id: run.id.clone(),
            seq: seq + 1,
            title: peer.title.clone(),
            instruction: peer.instruction.clone(),
            state: WorkUnitState::Implementing,
            assignee_role: AssigneeRole::Peer,
            parent_id: Some(master.id.clone()),
            allowed_globs: Some(peer.allowed_globs.clone()),
            depends_on: Vec::new(),
            test_command: None,
            local_key: Some(format!("peer-{seq}")),
            rework_count: 0,
            inner_commit_sha: None,
            receipt_id: None,
            verdict_id: None,
        };
        store.insert_work_unit(&peer_unit).await?;
        let _ = append_event(
            repo,
            &run.id,
            "inner_working",
            make_event(
                Actor::Master,
                EventKind::UnitSeeded,
                json!({
                    "work_unit_id": peer_unit.id,
                    "seq": peer_unit.seq,
                    "title": peer_unit.title,
                    "instruction": peer_unit.instruction,
                    "role": peer_unit.assignee_role.as_str(),
                    "parent_id": peer_unit.parent_id,
                    "local_key": peer_unit.local_key,
                    "allowed_globs": peer_unit.allowed_globs,
                }),
            ),
        );

        let fake_src = repo
            .join(".wanax")
            .join(format!("fake-peer-{seq}.toml"));
        if fake_src.is_file() {
            let dest_dir = peer_wt.join(".wanax");
            fs::create_dir_all(&dest_dir)
                .map_err(|e| WanaxError::with_detail(ErrorCode::WorkerCrash, e))?;
            fs::copy(&fake_src, dest_dir.join("fake.toml"))
                .map_err(|e| WanaxError::with_detail(ErrorCode::WorkerCrash, e))?;
        }

        fs::write(peer_wt.join("WORK_UNIT.md"), &peer.instruction)
            .map_err(|e| WanaxError::with_detail(ErrorCode::WorkerCrash, e))?;

        let peer_spec = sandbox_for(
            cfg,
            repo,
            &run.id,
            &peer_wt,
            &format!("peer-{seq}"),
            net,
        )?;
        let ctx = WorkerContext {
            run_id: run.id.clone(),
            work_unit_id: peer_unit.id.clone(),
            test_command: contract.test_command.clone(),
            test_timeout_secs: contract.test_timeout_secs,
            worktree: peer_wt.clone(),
            instruction: peer.instruction.clone(),
            adapter_name: adapter_kind.as_str().to_string(),
            extra_path: Some(wrapper_dir.to_path_buf()),
            timeout_secs: cfg.file.worker.timeout_secs,
            gitconfig: Some(gitconfig.to_path_buf()),
            sandbox: Some(peer_spec),
            proxy_url: proxy_url.map(String::from),
        };
        let spec_path = peer_wt.join(".wanax").join("fake.toml");
        let pr = PeerRun {
            unit: peer_unit,
            worktree: peer_wt,
            allowed_globs: peer.allowed_globs.clone(),
        };
        pending.push((pr, ctx, spec_path));
    }

    // Phase 2 (parallel): disjoint peers run their adapters concurrently.
    let mut kept = Vec::with_capacity(pending.len());
    let mut join = tokio::task::JoinSet::new();
    for (i, (pr, ctx, spec_path)) in pending.into_iter().enumerate() {
        let worker = cfg.file.worker.clone();
        kept.push(pr);
        join.spawn(async move {
            (i, start_adapter(adapter_kind, &worker, &ctx, &spec_path, 0, 1).await)
        });
    }
    let mut handles: Vec<Option<Result<wanax_worker::WorkerHandle, WanaxError>>> =
        (0..kept.len()).map(|_| None).collect();
    while let Some(joined) = join.join_next().await {
        match joined {
            Ok((i, res)) => handles[i] = Some(res),
            Err(e) => {
                return Err(WanaxError::with_detail(ErrorCode::WorkerCrash, e));
            }
        }
    }

    // Phase 3 (serial): account turns/cost, collect receipts in seq order.
    let mut peer_commits = Vec::new();
    for (pr, handle) in kept.into_iter().zip(handles) {
        let handle = handle
            .ok_or_else(|| WanaxError::from_code(ErrorCode::WorkerCrash))??;
        add_turns(run, handle.turns);
        charge_handle_usage(store, repo, run, cfg, &handle).await?;
        store.save_run_progress(run).await?;

        let peer_receipt = collect_peer_receipt(
            repo,
            &pr.worktree,
            contract,
            &pr.unit,
            adapter_kind.as_str(),
            &handle,
            base_sha,
            &pr.allowed_globs,
        )?;
        store.insert_receipt(&peer_receipt).await?;
        peer_commits.push(peer_receipt.commit_sha.clone());

        let _ = append_event(
            repo,
            &run.id,
            "inner_working",
            make_event(
                Actor::Peer,
                EventKind::ReceiptSubmitted,
                json!({
                    "work_unit_id": pr.unit.id,
                    "receipt_id": peer_receipt.id,
                    "commit_sha": peer_receipt.commit_sha,
                    "test_exit_code": peer_receipt.test_exit_code,
                    "claimed_pass": peer_receipt.claimed_pass,
                    "changed_files": peer_receipt.changed_files,
                }),
            ),
        );
    }

    for sha in &peer_commits {
        cherry_pick(inner_wt, sha).map_err(|e| {
            WanaxError::with_detail(
                ErrorCode::Db,
                format!("peer recovery blocked: {}", e.message),
            )
        })?;
    }

    let merged_sha = head_sha(inner_wt)?;
    let test = run_test_command(
        inner_wt,
        &contract.test_command,
        contract.test_timeout_secs,
        test_env,
        Some(inner_sandbox),
    )?;
    let test_exit = if test.timed_out { 124 } else { test.exit_code };
    let changed = diff_name_only(repo, base_sha, &merged_sha)?;
    let stat = diff_stat(repo, base_sha, &merged_sha)?;
    let receipt = Receipt {
        id: new_id(),
        work_unit_id: master.id.clone(),
        changed_files: changed,
        diffstat: stat,
        commit_sha: merged_sha,
        test_command: contract.test_command.clone(),
        test_exit_code: test_exit,
        test_excerpt: test.excerpt,
        claimed_pass: test_exit == 0,
        duration_ms: test.duration_ms,
        adapter: adapter_kind.as_str().to_string(),
        raw_artifact_path: None,
    };
    Ok(InnerPhaseResult {
        unit: master,
        receipt,
    })
}

fn peer_probe_paths(globs: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for g in globs {
        if g.contains('*') {
            let prefix = g.split('*').next().unwrap_or("").trim_end_matches('/');
            if prefix.is_empty() {
                out.push("file.rs".into());
            } else {
                out.push(format!("{prefix}/file.rs"));
            }
        } else {
            out.push(g.clone());
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn collect_peer_receipt(
    repo: &Path,
    peer_wt: &Path,
    contract: &Contract,
    unit: &WorkUnit,
    adapter: &str,
    handle: &wanax_worker::WorkerHandle,
    base_sha: &str,
    allowed: &[String],
) -> Result<Receipt, WanaxError> {
    let forbidden = compile_globs(&contract.forbidden_globs).ok();
    let mut to_add = Vec::new();
    for path in status_paths(peer_wt)? {
        if path.starts_with(".wanax/")
            || path.starts_with(".wanax-bin/")
            || path.starts_with("target/")
            || path == "WORK_UNIT.md"
            || path == "Cargo.lock"
        {
            continue;
        }
        if forbidden.as_ref().is_some_and(|g| g.is_match(&path)) {
            continue;
        }
        to_add.push(path);
    }
    let boundary = check_boundaries(&to_add, allowed, &contract.forbidden_globs)?;
    if !boundary.ok {
        return Err(WanaxError::from_code(ErrorCode::Boundary));
    }
    wanax_git::add_files(peer_wt, &to_add)?;
    let commit_sha = if to_add.is_empty() {
        head_sha(peer_wt)?
    } else {
        wanax_git::commit(peer_wt, &format!("wx({}): {}", unit.run_id, unit.title))?
    };
    let changed = diff_name_only(repo, base_sha, &commit_sha)?;
    let stat = diff_stat(repo, base_sha, &commit_sha)?;
    Ok(Receipt {
        id: new_id(),
        work_unit_id: unit.id.clone(),
        changed_files: changed,
        diffstat: stat,
        commit_sha,
        test_command: contract.test_command.clone(),
        test_exit_code: handle.test_exit_code,
        test_excerpt: handle.test_excerpt.clone(),
        claimed_pass: handle.claimed_pass,
        duration_ms: handle.duration_ms,
        adapter: adapter.to_string(),
        raw_artifact_path: handle.raw_artifact_path.clone(),
    })
}

fn copy_fake_specs(repo: &Path, dest_wt: &Path) {
    let src_dir = repo.join(".wanax");
    let dest = dest_wt.join(".wanax");
    let _ = fs::create_dir_all(&dest);
    let Ok(rd) = fs::read_dir(&src_dir) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let n = name.to_string_lossy();
        if n == "fake.toml" || (n.starts_with("fake-") && n.ends_with(".toml")) {
            let _ = fs::copy(entry.path(), dest.join(name));
        }
    }
}

fn dag_ready(u: &WorkUnit, all: &[WorkUnit]) -> bool {
    let done: std::collections::HashSet<&str> = all
        .iter()
        .filter(|d| d.state == WorkUnitState::Accepted)
        .map(|d| d.id.as_str())
        .collect();
    u.state != WorkUnitState::Accepted
        && u.state != WorkUnitState::Rejected
        && u.state != WorkUnitState::Blocked
        && u.depends_on.iter().all(|d| done.contains(d.as_str()))
}

/// Largest greedy subset of ready dag units whose allowed_globs are
/// pairwise disjoint — the wave that can safely run in parallel.
fn dag_wave(ready: &[WorkUnit]) -> Vec<WorkUnit> {
    let mut wave: Vec<WorkUnit> = Vec::new();
    for u in ready {
        let Some(globs) = &u.allowed_globs else {
            continue; // no declared boundary → never wave-parallel
        };
        let disjoint = wave.iter().all(|m| {
            let mg: &[String] = m.allowed_globs.as_deref().unwrap_or(&[]);
            !peer_glob_sets_overlap(globs, mg)
        });
        if disjoint {
            wave.push(u.clone());
        }
    }
    wave
}

fn next_ready_dag(units: &[WorkUnit]) -> Option<WorkUnit> {
    let done: std::collections::HashSet<&str> = units
        .iter()
        .filter(|u| u.state == WorkUnitState::Accepted)
        .map(|u| u.id.as_str())
        .collect();
    units
        .iter()
        .find(|u| {
            u.state != WorkUnitState::Accepted
                && u.state != WorkUnitState::Rejected
                && u.state != WorkUnitState::Blocked
                && u.depends_on.iter().all(|d| done.contains(d.as_str()))
        })
        .cloned()
}

async fn seed_dag_units(
    store: &Store,
    repo: &Path,
    run_id: &str,
    drafts: Vec<DagUnitDraft>,
) -> Result<Vec<WorkUnit>, WanaxError> {
    let nodes: Vec<(String, Vec<String>)> = drafts
        .iter()
        .map(|d| (d.id.clone(), d.depends_on.clone()))
        .collect();
    let order = wanax_core::dag::topo_sort(&nodes)?;
    let mut by_key = std::collections::HashMap::new();
    let mut created = Vec::new();
    for (idx, key) in order.iter().enumerate() {
        let draft = drafts
            .iter()
            .find(|d| d.id == *key)
            .ok_or_else(|| WanaxError::from_code(ErrorCode::CommanderSchema))?;
        let unit = WorkUnit {
            id: new_id(),
            run_id: run_id.to_string(),
            seq: u32::try_from(idx + 1).unwrap_or(1),
            title: draft.title.clone(),
            instruction: draft.instruction.clone(),
            state: WorkUnitState::Queued,
            assignee_role: AssigneeRole::Goal,
            parent_id: None,
            allowed_globs: draft.allowed_globs.clone(),
            depends_on: Vec::new(),
            test_command: draft.test_command.clone(),
            local_key: Some(draft.id.clone()),
            rework_count: 0,
            inner_commit_sha: None,
            receipt_id: None,
            verdict_id: None,
        };
        by_key.insert(draft.id.clone(), unit.id.clone());
        created.push((draft.depends_on.clone(), unit));
    }
    let mut out = Vec::new();
    for (deps, mut unit) in created {
        unit.depends_on = deps
            .iter()
            .filter_map(|k| by_key.get(k).cloned())
            .collect();
        store.insert_work_unit(&unit).await?;
        // Replay-complete evidence: the envelope alone must be enough to
        // rebuild the dag set after a crash.
        let _ = append_event(
            repo,
            run_id,
            "dispatched",
            make_event(
                Actor::Commander,
                EventKind::UnitSeeded,
                json!({
                    "work_unit_id": unit.id,
                    "seq": unit.seq,
                    "title": unit.title,
                    "instruction": unit.instruction,
                    "role": unit.assignee_role.as_str(),
                    "local_key": unit.local_key,
                    "depends_on": unit.depends_on,
                    "allowed_globs": unit.allowed_globs,
                    "test_command": unit.test_command,
                }),
            ),
        );
        out.push(unit);
    }
    Ok(out)
}

struct GoalLoopParams<'a> {
    store: &'a Store,
    repo: &'a Path,
    run: &'a mut FactoryRun,
    contract: &'a Contract,
    cfg: &'a ResolvedConfig,
    inner_wt: &'a Path,
    adapter_kind: WorkerAdapterKind,
    spec_path: &'a Path,
    wrapper_dir: &'a Path,
    unit: &'a WorkUnit,
    review_client: Option<&'a dyn wanax_llm::CompletionClient>,
    gitconfig: &'a Path,
    test_env: &'a HashMap<String, String>,
    inner_sandbox: &'a SandboxSpec,
    proxy_url: Option<&'a str>,
}

async fn run_goal_loop(
    params: GoalLoopParams<'_>,
) -> Result<wanax_worker::WorkerHandle, WanaxError> {
    let GoalLoopParams {
        store,
        repo,
        run,
        contract,
        cfg,
        inner_wt,
        adapter_kind,
        spec_path,
        wrapper_dir,
        unit,
        review_client,
        gitconfig,
        test_env,
        inner_sandbox,
        proxy_url,
    } = params;
    let ctx = WorkerContext {
        run_id: run.id.clone(),
        work_unit_id: unit.id.clone(),
        test_command: contract.test_command.clone(),
        test_timeout_secs: contract.test_timeout_secs,
        worktree: inner_wt.to_path_buf(),
        instruction: unit.instruction.clone(),
        adapter_name: adapter_kind.as_str().to_string(),
        extra_path: Some(wrapper_dir.to_path_buf()),
        timeout_secs: cfg.file.worker.timeout_secs,
        gitconfig: Some(gitconfig.to_path_buf()),
        sandbox: Some(inner_sandbox.clone()),
        proxy_url: proxy_url.map(String::from),
    };
    let mut last_handle = None;
    let mut prev_paths: Option<Vec<String>> = None;
    for iter in 1..=MAX_GOAL_ITERS {
        if is_budget_exhausted(run) {
            break;
        }
        let _ = append_event(
            repo,
            &run.id,
            "inner_working",
            make_event(
                Actor::Goal,
                EventKind::StateChanged,
                json!({ "phase": "plan", "iter": iter }),
            ),
        );
        let before = goal_status_paths(inner_wt)?;
        let handle = start_adapter(
            adapter_kind,
            &cfg.file.worker,
            &ctx,
            spec_path,
            unit.rework_count,
            iter,
        )
        .await?;
        add_turns(run, handle.turns);
        charge_handle_usage(store, repo, run, cfg, &handle).await?;
        store.save_run_progress(run).await?;
        if handle.crashed {
            return Ok(handle);
        }

        let test = run_test_command(
            inner_wt,
            &contract.test_command,
            contract.test_timeout_secs,
            test_env,
            Some(inner_sandbox),
        )?;
        let inner_code = if test.timed_out { 124 } else { test.exit_code };
        let mut handle = handle;
        handle.test_exit_code = inner_code;
        handle.test_excerpt = test.excerpt.clone();
        handle.duration_ms = test.duration_ms;

        let changed = goal_status_paths(inner_wt)?;
        let review = run_self_review(
            run.reviewer_model.as_deref(),
            &run.inner_model,
            review_client,
            &changed,
            inner_code,
            &test.excerpt,
        )
        .await;
        if let Some(usage) = &review.usage {
            charge_and_tick(
                store,
                repo,
                run,
                usage,
                cfg.inner_in_micros,
                cfg.inner_out_micros,
            )
            .await?;
        }
        let _ = append_event(
            repo,
            &run.id,
            "inner_working",
            make_event(
                Actor::Goal,
                EventKind::StateChanged,
                json!({
                    "phase": "self_review",
                    "iter": iter,
                    "self_review": if review.degraded { "degraded" } else { "semantic" },
                    "mode": review.mode,
                    "test_exit_code": inner_code,
                }),
            ),
        );
        last_handle = Some(handle);
        if inner_code == 0 || is_budget_exhausted(run) {
            break;
        }
        if changed == before || prev_paths.as_ref() == Some(&changed) {
            break;
        }
        prev_paths = Some(changed);
    }
    last_handle.ok_or_else(|| WanaxError::from_code(ErrorCode::WorkerCrash))
}

fn goal_status_paths(inner_wt: &Path) -> Result<Vec<String>, WanaxError> {
    let mut paths: Vec<String> = status_paths(inner_wt)?
        .into_iter()
        .filter(|p| {
            !p.starts_with(".wanax/")
                && !p.starts_with(".wanax-bin/")
                && !p.starts_with("target/")
                && p != "WORK_UNIT.md"
                && p != "Cargo.lock"
        })
        .collect();
    paths.sort();
    Ok(paths)
}

async fn start_adapter(
    adapter_kind: WorkerAdapterKind,
    worker: &WorkerConfig,
    ctx: &WorkerContext,
    spec_path: &Path,
    rework_count: u32,
    goal_iter: u32,
) -> Result<wanax_worker::WorkerHandle, WanaxError> {
    match adapter_kind {
        WorkerAdapterKind::Fake => {
            let mut fake = FakeAdapter::new(rework_count);
            fake.goal_iter = goal_iter;
            if spec_path.is_file() {
                fake.spec_path = Some(spec_path.to_path_buf());
            }
            fake.start(ctx).await
        }
        WorkerAdapterKind::Octoscode => {
            OctoscodeAdapter::new(&worker.octoscode_bin)
                .start(ctx)
                .await
        }
        WorkerAdapterKind::Cmd => {
            CmdAdapter::new(worker.cmd.clone(), worker.cmd_args.clone())
                .start(ctx)
                .await
        }
        WorkerAdapterKind::Claude => {
            ClaudeAdapter::new(worker.claude_bin.clone(), worker.claude_args.clone())
                .start(ctx)
                .await
        }
        WorkerAdapterKind::Codex => {
            CodexAdapter::new(worker.codex_bin.clone(), worker.codex_args.clone())
                .start(ctx)
                .await
        }
        WorkerAdapterKind::Acp => {
            AcpAdapter::new(worker.acp_bin.clone(), worker.acp_args.clone())
                .start(ctx)
                .await
        }
    }
}

pub(crate) fn preflight_adapter(adapter_kind: WorkerAdapterKind, cfg: &ResolvedConfig) -> Result<(), WanaxError> {
    if !adapter_kind.is_supported() {
        return Err(WanaxError::new(
            ErrorCode::AdapterMissing,
            format!("adapter binary not found: {}", adapter_kind.as_str()),
        ));
    }
    match adapter_kind {
        WorkerAdapterKind::Octoscode => {
            let octo = OctoscodeAdapter::new(&cfg.file.worker.octoscode_bin);
            octo.resolve_bin()?;
            if !octo.has_yolo_flag()? {
                return Err(WanaxError::new(
                    ErrorCode::AdapterMissing,
                    format!(
                        "adapter binary not found: {} (--yolo missing)",
                        cfg.file.worker.octoscode_bin
                    ),
                ));
            }
            Ok(())
        }
        WorkerAdapterKind::Cmd => CmdAdapter::new(
            cfg.file.worker.cmd.clone(),
            cfg.file.worker.cmd_args.clone(),
        )
        .resolve_bin()
        .map(|_| ()),
        WorkerAdapterKind::Claude => ClaudeAdapter::new(
            cfg.file.worker.claude_bin.clone(),
            cfg.file.worker.claude_args.clone(),
        )
        .resolve_bin()
        .map(|_| ()),
        WorkerAdapterKind::Codex => CodexAdapter::new(
            cfg.file.worker.codex_bin.clone(),
            cfg.file.worker.codex_args.clone(),
        )
        .resolve_bin()
        .map(|_| ()),
        WorkerAdapterKind::Acp => AcpAdapter::new(
            cfg.file.worker.acp_bin.clone(),
            cfg.file.worker.acp_args.clone(),
        )
        .resolve_bin()
        .map(|_| ()),
        WorkerAdapterKind::Fake => Ok(()),
    }
}

/// Environment for spawned test harnesses: whitelist base + contract-declared
/// pass_env + the wanax-managed git config (never the wanax process env).
fn test_child_env(
    cfg: &ResolvedConfig,
    gitconfig: &Path,
    proxy_url: Option<&str>,
) -> HashMap<String, String> {
    let mut env = envenv::base_child_env();
    envenv::with_passthrough(&mut env, &cfg.file.test.pass_env);
    envenv::with_git_config(&mut env, gitconfig);
    if let Some(url) = proxy_url {
        for (k, v) in wanax_core::egress::proxy_env_from_url(url) {
            env.insert(k, v);
        }
    }
    env
}

/// Resolved egress confinement for one run: the allowlist proxy stays alive
/// as long as `proxy` is held — drop aborts its task.
struct EgressPlan {
    mode: wanax_core::egress::EgressMode,
    net: wanax_core::sandbox::NetConfine,
    proxy_url: Option<String>,
    n_hosts: usize,
    _proxy: Option<wanax_core::egress::EgressProxy>,
}

impl EgressPlan {
    fn label(&self) -> String {
        match self.mode {
            wanax_core::egress::EgressMode::Allowlist => {
                format!("allowlist:{}", self.n_hosts)
            }
            m => m.as_str().to_string(),
        }
    }
}

/// `[worker] egress` → proxy + env + sandbox net rule. The allowlist unions
/// `egress_allow` with the provider base_url hosts so a worker can always
/// reach its own API — and nothing else.
async fn build_egress(cfg: &ResolvedConfig) -> Result<EgressPlan, WanaxError> {
    use wanax_core::egress::{EgressAllowlist, EgressMode, EgressProxy};
    use wanax_core::sandbox::NetConfine;
    let mode = EgressMode::parse(&cfg.file.worker.egress);
    match mode {
        EgressMode::Off => Ok(EgressPlan {
            mode,
            net: NetConfine::Open,
            proxy_url: None,
            n_hosts: 0,
            _proxy: None,
        }),
        EgressMode::Isolated => Ok(EgressPlan {
            mode,
            net: NetConfine::Isolated,
            proxy_url: None,
            n_hosts: 0,
            _proxy: None,
        }),
        EgressMode::Allowlist => {
            let mut hosts = cfg.file.worker.egress_allow.clone();
            for base in [&cfg.file.commander.base_url, &cfg.file.inner.base_url]
                .into_iter()
                .flatten()
            {
                if let Some(h) = wanax_core::egress::host_of_base_url(base) {
                    hosts.push(h);
                }
            }
            let allow = EgressAllowlist::new(hosts);
            let proxy = EgressProxy::start(
                allow.clone(),
                std::sync::Arc::new(|m| tracing::warn!("{m}")),
            )
            .await
            .map_err(|e| WanaxError::with_detail(ErrorCode::WorkerCrash, e))?;
            let port = proxy.port;
            Ok(EgressPlan {
                mode,
                net: NetConfine::ProxyOnly(port),
                proxy_url: Some(format!("http://127.0.0.1:{port}")),
                n_hosts: allow.len(),
                _proxy: Some(proxy),
            })
        }
    }
}

/// Confinement spec for one spawned role (inner worker, peer, outer retest,
/// baseline): the worktree + real git dir + run dir are writable; everything
/// else read-only; secret dirs unreadable.
fn sandbox_for(
    cfg: &ResolvedConfig,
    repo: &Path,
    run_id: &str,
    worktree: &Path,
    role: &str,
    net: wanax_core::sandbox::NetConfine,
) -> Result<SandboxSpec, WanaxError> {
    let rd = run_dir(repo, run_id);
    let git_dir = git_common_dir(repo).unwrap_or_else(|_| repo.join(".git"));
    sandbox::resolve(
        &cfg.file.sandbox.mode,
        vec![worktree.to_path_buf(), git_dir, rd.clone()],
        &cfg.file.sandbox.extra_write_paths,
        Some(rd.join(format!("sandbox-{role}.sb"))),
        net,
    )
}

fn baseline_exit_from_envelope(repo: &Path, run_id: &str) -> Option<i32> {
    let env = wanax_tombstone::load_envelope(repo, run_id).ok()?;
    env.events
        .iter()
        .rev()
        .find(|e| e.kind == wanax_tombstone::EventKind::BaselineTest)
        .and_then(|e| e.payload.get("exit_code").and_then(|v| v.as_i64()))
        .map(|c| c as i32)
}

/// Real worker cost/usage (Claude `total_cost_usd`, Codex token usage) → budget.
async fn charge_handle_usage(
    store: &Store,
    repo: &Path,
    run: &mut FactoryRun,
    cfg: &ResolvedConfig,
    handle: &wanax_worker::WorkerHandle,
) -> Result<(), WanaxError> {
    if handle.cost_usd_micros.is_none() && handle.usage_in.is_none() && handle.usage_out.is_none()
    {
        return Ok(());
    }
    let usage = LlmUsage {
        usd_micros: handle.cost_usd_micros,
        prompt_tokens: handle.usage_in,
        completion_tokens: handle.usage_out,
        ..LlmUsage::default()
    };
    charge_and_tick(
        store,
        repo,
        run,
        &usage,
        cfg.inner_in_micros,
        cfg.inner_out_micros,
    )
    .await
}

async fn charge_and_tick(
    store: &Store,
    repo: &Path,
    run: &mut FactoryRun,
    usage: &LlmUsage,
    rate_in: i64,
    rate_out: i64,
) -> Result<(), WanaxError> {
    let tick = if let Some(micros) = usage.usd_micros {
        charge_usd_micros(run, micros)
    } else {
        let (units_in, units_out, estimated) = usage.charge_units();
        charge_units(run, units_in, units_out, rate_in, rate_out, estimated)
    };
    store.save_run_progress(run).await?;
    let mut payload = json!({
        "spent_usd_micros": tick.spent_usd_micros,
        "spent_inner_turns": tick.spent_inner_turns,
        "cost_estimated": tick.cost_estimated,
    });
    if let Some(m) = usage.usd_micros {
        payload["charge_usd_micros"] = json!(m);
    }
    if let Some(tokens) = usage.prompt_tokens {
        payload["prompt_tokens"] = json!(tokens);
    }
    if let Some(tokens) = usage.completion_tokens {
        payload["completion_tokens"] = json!(tokens);
    }
    let _ = append_event(
        repo,
        &run.id,
        run.state.as_str(),
        make_event(Actor::System, EventKind::BudgetTick, payload),
    );
    Ok(())
}

async fn fail_budget(store: &Store, repo: &Path, run: &mut FactoryRun) -> Result<(), WanaxError> {
    let err = budget_error(run);
    store
        .set_state(
            &mut *run,
            RunState::BudgetExhausted,
            Some(err.message.clone()),
        )
        .await?;
    println!("state={}", run.state.as_str());
    let _ = append_event(
        repo,
        &run.id,
        "budget_exhausted",
        make_event(
            Actor::System,
            EventKind::Error,
            json!({ "code": ErrorCode::Budget.as_str(), "message": err.message }),
        ),
    );
    Err(err)
}

async fn fail_run(
    store: &Store,
    repo: &Path,
    run: &mut FactoryRun,
    err: WanaxError,
) -> Result<(), WanaxError> {
    store
        .set_state(&mut *run, RunState::Failed, Some(err.message.clone()))
        .await?;
    println!("state={}", run.state.as_str());
    let _ = append_event(
        repo,
        &run.id,
        "failed",
        make_event(
            Actor::System,
            EventKind::Error,
            json!({ "code": err.code.as_str(), "message": err.message }),
        ),
    );
    Err(err)
}

fn collect_inner_receipt(
    repo: &Path,
    inner_wt: &Path,
    contract: &Contract,
    unit: &WorkUnit,
    adapter: &str,
    handle: &wanax_worker::WorkerHandle,
    base_sha: &str,
) -> Result<Receipt, WanaxError> {
    let forbidden = compile_globs(&contract.forbidden_globs).ok();
    let mut to_add = Vec::new();
    let mut skipped = Vec::new();
    for path in status_paths(inner_wt)? {
        if path.starts_with(".wanax/")
            || path.starts_with(".wanax-bin/")
            || path.starts_with("target/")
            || path == "WORK_UNIT.md"
            || path == "Cargo.lock"
        {
            continue;
        }
        if forbidden.as_ref().is_some_and(|g| g.is_match(&path)) {
            skipped.push(path);
            continue;
        }
        to_add.push(path);
    }
    if !skipped.is_empty() {
        tracing::warn!("boundary_violation skipped {:?}", skipped);
    }
    wanax_git::add_files(inner_wt, &to_add)?;
    let after = status_paths(inner_wt)?;
    let staged: Vec<String> = after
        .into_iter()
        .filter(|p| !p.starts_with(".wanax") && p != "WORK_UNIT.md")
        .collect();
    let commit_sha = if staged.is_empty() && to_add.is_empty() {
        head_sha(inner_wt)?
    } else {
        wanax_git::commit(inner_wt, &format!("wx({}): {}", unit.run_id, unit.title))?
    };
    let changed = diff_name_only(repo, base_sha, &commit_sha)?;
    let stat = diff_stat(repo, base_sha, &commit_sha)?;
    Ok(Receipt {
        id: new_id(),
        work_unit_id: unit.id.clone(),
        changed_files: changed,
        diffstat: stat,
        commit_sha,
        test_command: contract.test_command.clone(),
        test_exit_code: handle.test_exit_code,
        test_excerpt: handle.test_excerpt.clone(),
        claimed_pass: handle.claimed_pass,
        duration_ms: handle.duration_ms,
        adapter: adapter.to_string(),
        raw_artifact_path: handle.raw_artifact_path.clone(),
    })
}

fn write_result(
    repo: &Path,
    run_id: &str,
    decision: &str,
    sha: &str,
    excerpt: &str,
    spent_usd_micros: i64,
) -> Result<(), WanaxError> {
    let path = run_dir(repo, run_id).join("RESULT.md");
    let body = format!(
        "# Result\n\ndecision: {decision}\nsha: {sha}\nspent_usd: {}\n\n```\n{excerpt}\n```\n",
        wanax_core::money::format_usd_4(spent_usd_micros)
    );
    fs::write(path, body).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    Ok(())
}
