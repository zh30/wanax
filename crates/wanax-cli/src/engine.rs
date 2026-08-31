use crate::logging;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use wanax_core::accept::{enforce_decision, AcceptGates};
use wanax_core::budget::{add_turns, budget_error, charge_units, is_budget_exhausted};
use wanax_core::config::load_merged_config;
use wanax_core::contract::parse_contract_file;
use wanax_core::error::{ErrorCode, WanaxError};
use wanax_core::hashutil::sha256_file;
use wanax_core::ids::new_id;
use wanax_core::lock::RepoLock;
use wanax_core::timeutil::now_rfc3339;
use wanax_core::types::{
    inner_branch_name, outer_branch_name, AssigneeRole, Contract, FactoryRun, Receipt, RunState,
    Verdict, VerdictDecision, WorkUnit, WorkUnitState, WorkerAdapterKind, MAX_GOAL_ITERS,
    MAX_REWORK,
};
use wanax_core::{ResolvedConfig, Store};
use wanax_git::{
    create_branch, diff_name_only, diff_stat, dirty_non_wanax, harden_inner_worktree, head_sha,
    install_git_wrapper, is_ancestor, repo_root, require_git_repo, status_paths,
    worktree_add_branch, worktree_add_detach,
};
use wanax_llm::{
    dispatch_with_retry, pick_commander, pick_review_client, run_self_review, Commander,
    DispatchContext, LlmUsage, VerdictContext,
};
use wanax_tombstone::{
    append_event, init_envelope, make_event, persist_envelope, run_dir, Actor, EventKind,
};
use wanax_verify::{check_boundaries, compile_globs, run_test_command};
use wanax_worker::{FakeAdapter, OctoscodeAdapter, WorkerAdapter, WorkerContext};

pub struct StartOpts {
    pub contract: PathBuf,
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
    if !adapter_kind.is_phase1() {
        return Err(WanaxError::new(
            ErrorCode::AdapterMissing,
            format!("adapter binary not found: {}", adapter_kind.as_str()),
        ));
    }
    if adapter_kind == WorkerAdapterKind::Octoscode {
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
    }

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

    if !opts.allow_dirty {
        let dirty = dirty_non_wanax(&repo)?;
        if !dirty.is_empty() {
            return Err(WanaxError::from_code(ErrorCode::DirtyWorktree));
        }
    }

    fs::create_dir_all(opts.data_dir.join("."))
        .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    let store = Store::open(&opts.data_dir.join("wanax.db")).await?;

    let run_id = new_id();
    let lock = RepoLock::acquire(&repo, &run_id)?;

    let result = run_factory(
        &store,
        &repo,
        contract,
        contract_abs,
        adapter_kind,
        &cfg,
        run_id,
    )
    .await;
    let _ = lock.release();
    result
}

#[allow(clippy::too_many_lines)]
async fn run_factory(
    store: &Store,
    repo: &Path,
    mut contract: Contract,
    contract_abs: PathBuf,
    adapter_kind: WorkerAdapterKind,
    cfg: &ResolvedConfig,
    run_id: String,
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

    let fake_src = repo.join(".wanax").join("fake.toml");
    if fake_src.is_file() {
        let dest_dir = inner_wt.join(".wanax");
        let _ = fs::create_dir_all(&dest_dir);
        let _ = fs::copy(&fake_src, dest_dir.join("fake.toml"));
    }

    logging::init_run_log(&run_dir(repo, &run.id).join("wanax.log")).ok();

    let started = make_event(
        Actor::System,
        EventKind::RunStarted,
        json!({ "base_sha": base_sha }),
    );
    init_envelope(repo, &run.id, &run.contract_sha256, "dispatched", started)?;
    store
        .set_state(&mut run, RunState::Dispatched, None)
        .await?;
    println!("state={}", run.state.as_str());

    let commander: Box<dyn Commander> = pick_commander(cfg)?;
    let review_client = pick_review_client(cfg);

    let mut unit: Option<WorkUnit> = None;
    let mut rework_notes: Option<String> = None;
    let mut outer_n = 0u32;

    let outcome = loop {
        if is_budget_exhausted(&run) {
            break fail_budget(store, repo, &mut run).await;
        }

        let draft = match dispatch_with_retry(
            commander.as_ref(),
            &DispatchContext {
                contract: contract.clone(),
                rework_notes: rework_notes.clone(),
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
            &draft.1,
            cfg.commander_in_micros,
            cfg.commander_out_micros,
        )
        .await?;
        if is_budget_exhausted(&run) {
            break fail_budget(store, repo, &mut run).await;
        }

        let u = match unit.as_mut() {
            Some(existing) => {
                existing.instruction = draft.0.instruction;
                existing.title = draft.0.title;
                existing.state = WorkUnitState::Assigned;
                store.update_work_unit(existing).await?;
                existing.clone()
            }
            None => {
                let created = WorkUnit {
                    id: new_id(),
                    run_id: run.id.clone(),
                    seq: 1,
                    title: draft.0.title,
                    instruction: draft.0.instruction,
                    state: WorkUnitState::Assigned,
                    assignee_role: AssigneeRole::Goal,
                    parent_id: None,
                    rework_count: 0,
                    inner_commit_sha: None,
                    receipt_id: None,
                    verdict_id: None,
                };
                store.insert_work_unit(&created).await?;
                unit = Some(created.clone());
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
                json!({
                    "work_unit_id": u.id,
                    "title": u.title,
                    "instruction": u.instruction,
                }),
            ),
        );
        store
            .set_state(&mut run, RunState::InnerWorking, None)
            .await?;
        println!("state={}", run.state.as_str());

        let wu_path = inner_wt.join("WORK_UNIT.md");
        fs::write(&wu_path, &u.instruction)
            .map_err(|e| WanaxError::with_detail(ErrorCode::WorkerCrash, e))?;

        if let Some(existing) = unit.as_mut() {
            existing.assignee_role = AssigneeRole::Goal;
            existing.state = WorkUnitState::Implementing;
            store.update_work_unit(existing).await?;
        }

        run.worker_pid = Some(i64::from(std::process::id()));
        store.save_run_progress(&run).await?;
        println!("starting worker pid={}", std::process::id());

        let spec_path = inner_wt.join(".wanax").join("fake.toml");
        let handle = match run_goal_loop(GoalLoopParams {
            store,
            repo,
            run: &mut run,
            contract: &contract,
            cfg,
            inner_wt: &inner_wt,
            adapter_kind,
            spec_path: &spec_path,
            wrapper_dir: &wrapper_dir,
            unit: &u,
            review_client: review_client.as_deref(),
        })
        .await
        {
            Ok(h) => h,
            Err(e) => break fail_run(store, repo, &mut run, e).await,
        };

        if run.spent_inner_turns >= run.max_inner_turns && handle.crashed {
            break fail_budget(store, repo, &mut run).await;
        }

        let receipt = match collect_inner_receipt(
            repo,
            &inner_wt,
            &contract,
            &u,
            adapter_kind.as_str(),
            &handle,
            &base_sha,
        ) {
            Ok(r) => r,
            Err(e) => break fail_run(store, repo, &mut run, e).await,
        };
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
                    "claimed_pass": receipt.claimed_pass,
                    "changed_files": receipt.changed_files,
                    "commit_sha": receipt.commit_sha,
                }),
            ),
        );
        store
            .set_state(&mut run, RunState::ReceiptReady, None)
            .await?;
        println!("state={}", run.state.as_str());

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

        let test = run_test_command(
            &outer_wt,
            &contract.test_command,
            contract.test_timeout_secs,
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

        let descendant = is_ancestor(repo, &base_sha, &receipt.commit_sha)?;
        let budget_done = is_budget_exhausted(&run);
        let current_rework = unit.as_ref().map(|u| u.rework_count).unwrap_or(0);
        let gates = AcceptGates::from_parts(
            &receipt,
            &contract.test_command,
            outer_code,
            boundary.ok,
            descendant,
            budget_done,
            current_rework,
        );

        let vctx = VerdictContext {
            contract: contract.clone(),
            receipt: receipt.clone(),
            diffstat: receipt.diffstat.clone(),
            changed_files: changed.clone(),
            outer_test_exit_code: outer_code,
            outer_test_excerpt: test.excerpt.clone(),
            boundary_ok: boundary.ok,
            rework_count: current_rework,
        };
        let (proposed, usage) = {
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
                    "outer_test_exit_code": outer_code,
                    "boundary_ok": boundary.ok,
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
                store.set_state(&mut run, RunState::Accepted, None).await?;
                println!("state={}", run.state.as_str());
                write_result(repo, &run.id, "accept", &receipt.commit_sha, &test.excerpt)?;
                println!("{}", run.inner_branch);
                if let Ok(stat) = diff_stat(repo, &base_sha, &receipt.commit_sha) {
                    println!("{stat}");
                }
                break Ok(());
            }
            VerdictDecision::Reject => {
                store.set_state(&mut run, RunState::Rejected, None).await?;
                println!("state={}", run.state.as_str());
                write_result(repo, &run.id, "reject", &receipt.commit_sha, &test.excerpt)?;
                break Err(WanaxError::new(
                    if boundary.ok {
                        ErrorCode::WorkerCrash
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
                    .set_state(
                        &mut run,
                        RunState::Escalate,
                        Some(ErrorCode::ReworkLimit.default_message().into()),
                    )
                    .await?;
                println!("state={}", run.state.as_str());
                break Err(WanaxError::from_code(ErrorCode::ReworkLimit));
            }
            VerdictDecision::Rework => {
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
                    "Previous attempt failed.\nexit={outer_code}\n{}\n",
                    test.excerpt
                ));
                store.set_state(&mut run, RunState::Rework, None).await?;
                println!("state={}", run.state.as_str());
                store
                    .set_state(&mut run, RunState::Dispatched, None)
                    .await?;
                println!("state={}", run.state.as_str());
            }
        }

        let _ = sha256_file(&contract_abs);
    };

    if let Ok(mut env) = wanax_tombstone::load_envelope(repo, &run.id) {
        env.current_state = run.state.as_str().to_string();
        let _ = persist_envelope(repo, &env);
    }
    outcome
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
    };
    let mut last_handle = None;
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
        let handle =
            start_adapter(adapter_kind, cfg, &ctx, spec_path, unit.rework_count, iter).await?;
        add_turns(run, handle.turns);
        store.save_run_progress(run).await?;
        if handle.crashed {
            return Ok(handle);
        }

        let test = run_test_command(inner_wt, &contract.test_command, contract.test_timeout_secs)?;
        let inner_code = if test.timed_out { 124 } else { test.exit_code };
        let mut handle = handle;
        handle.test_exit_code = inner_code;
        handle.test_excerpt = test.excerpt.clone();

        let changed: Vec<String> = status_paths(inner_wt)?
            .into_iter()
            .filter(|p| {
                !p.starts_with(".wanax/")
                    && !p.starts_with(".wanax-bin/")
                    && !p.starts_with("target/")
                    && p != "WORK_UNIT.md"
                    && p != "Cargo.lock"
            })
            .collect();
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
    }
    last_handle.ok_or_else(|| WanaxError::from_code(ErrorCode::WorkerCrash))
}

async fn start_adapter(
    adapter_kind: WorkerAdapterKind,
    cfg: &ResolvedConfig,
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
            OctoscodeAdapter::new(&cfg.file.worker.octoscode_bin)
                .start(ctx)
                .await
        }
        _ => Err(WanaxError::new(
            ErrorCode::AdapterMissing,
            format!("adapter binary not found: {}", adapter_kind.as_str()),
        )),
    }
}

async fn charge_and_tick(
    store: &Store,
    repo: &Path,
    run: &mut FactoryRun,
    usage: &LlmUsage,
    rate_in: i64,
    rate_out: i64,
) -> Result<(), WanaxError> {
    let (units_in, units_out, estimated) = usage.charge_units();
    let tick = charge_units(run, units_in, units_out, rate_in, rate_out, estimated);
    store.save_run_progress(run).await?;
    let mut payload = json!({
        "spent_usd_micros": tick.spent_usd_micros,
        "spent_inner_turns": tick.spent_inner_turns,
        "cost_estimated": tick.cost_estimated,
    });
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
) -> Result<(), WanaxError> {
    let path = run_dir(repo, run_id).join("RESULT.md");
    let body = format!("# Result\n\ndecision: {decision}\nsha: {sha}\n\n```\n{excerpt}\n```\n");
    fs::write(path, body).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    Ok(())
}
