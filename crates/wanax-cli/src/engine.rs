use crate::gh::maybe_create_github_pr;
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
    inner_branch_name, outer_branch_name, peer_branch_name, AssigneeRole, Contract, FactoryRun,
    Receipt, RunState, Verdict, VerdictDecision, WorkUnit, WorkUnitState, WorkerAdapterKind,
    MAX_GOAL_ITERS, MAX_REWORK,
};
use wanax_core::{ResolvedConfig, Store};
use wanax_git::{
    cherry_pick, create_branch, diff_name_only, diff_stat, dirty_non_wanax, harden_inner_worktree,
    head_sha, install_git_wrapper, is_ancestor, ref_exists, repo_root, require_git_repo,
    status_paths, worktree_add_branch, worktree_add_detach,
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
use wanax_worker::{CmdAdapter, FakeAdapter, OctoscodeAdapter, WorkerAdapter, WorkerContext};

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
    if adapter_kind == WorkerAdapterKind::Cmd {
        CmdAdapter::new(
            cfg.file.worker.cmd.clone(),
            cfg.file.worker.cmd_args.clone(),
        )
        .resolve_bin()?;
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
    if !adapter_kind.is_phase1() {
        return Err(WanaxError::new(
            ErrorCode::AdapterMissing,
            format!("adapter binary not found: {}", adapter_kind.as_str()),
        ));
    }
    let contract = store.get_contract(&run.contract_id).await?;
    let contract_abs = repo.join(&contract.path);
    let lock = RepoLock::acquire_for_resume(&repo, &run.id)?;
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

    copy_fake_specs(repo, &inner_wt);

    logging::init_run_log(&run_dir(repo, &run.id).join("wanax.log")).ok();

    let started = make_event(
        Actor::System,
        EventKind::RunStarted,
        json!({ "base_sha": base_sha }),
    );
    init_envelope(repo, &run.id, &run.contract_sha256, "dispatched", started)?;
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

    drive_factory(DriveParams {
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
    })
    .await
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

    let existing = store.work_units_for_run(&run.id).await?;
    let dag_units = existing
        .iter()
        .any(|u| u.local_key.is_some() || !u.depends_on.is_empty())
        .then(|| existing.clone());
    let unit = existing.last().cloned();
    let mut pending_outer = None;
    if matches!(
        run.state,
        RunState::ReceiptReady | RunState::OuterReviewing
    ) {
        if let Some(u) = unit.clone() {
            if let Some(rid) = &u.receipt_id {
                if let Ok(receipt) = store.get_receipt(rid).await {
                    pending_outer = Some((u, receipt));
                }
            }
        }
    }

    drive_factory(DriveParams {
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
    })
    .await
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
    } = params;
    let commander: Box<dyn Commander> = pick_commander(cfg)?;
    let review_client = pick_review_client(cfg);
    let mut rework_notes: Option<String> = None;
    let mut reuse_existing = unit.is_some() && pending_outer.is_none() && dag_units.is_none();

    let outcome = loop {
        if is_budget_exhausted(&run) {
            break fail_budget(store, repo, &mut run).await;
        }

        let mut receipt_already_stored = false;
        let inner_result = if let Some((u0, r0)) = pending_outer.take() {
            receipt_already_stored = true;
            Ok(InnerPhaseResult {
                unit: u0,
                receipt: r0,
            })
        } else if let Some(ref dags) = dag_units {
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
            })
            .await
        } else {
            let (plan, draft_usage) = match dispatch_with_retry(
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
                    })
                    .await
                }
                DispatchPlan::Dag(drafts) => {
                    match seed_dag_units(store, &run.id, drafts).await {
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
        let test = run_test_command(&outer_wt, test_cmd, contract.test_timeout_secs)?;
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
        let current_rework = unit.as_ref().map(|u| u.rework_count).unwrap_or(0);
        let gates = AcceptGates::from_parts(
            &receipt,
            test_cmd,
            outer_code,
            boundary.ok,
            descendant,
            budget_done,
            current_rework,
        )
        .with_plugin(plugin.ok);

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
                write_result(repo, &run.id, "accept", &receipt.commit_sha, &test.excerpt)?;
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
            json!({
                "work_unit_id": u.id,
                "title": u.title,
                "instruction": u.instruction,
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
                "mode": "peer_batch",
                "peer_count": peers.len(),
            }),
        ),
    );
    store
        .set_state(run, RunState::InnerWorking, None)
        .await?;
    println!("state={}", run.state.as_str());

    run.worker_pid = Some(i64::from(std::process::id()));
    store.save_run_progress(run).await?;

    let mut peer_commits = Vec::new();
    for (idx, peer) in peers.iter().enumerate() {
        let seq = u32::try_from(idx + 1).unwrap_or(1);
        let branch = peer_branch_name(&run.id, seq);
        create_branch(repo, &branch, base_sha)?;
        let peer_wt = repo
            .join(".wanax")
            .join("worktrees")
            .join(format!("{}-peer-{seq}", run.id));
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
        };
        let spec_path = peer_wt.join(".wanax").join("fake.toml");
        let handle = start_adapter(adapter_kind, cfg, &ctx, &spec_path, 0, 1).await?;
        add_turns(run, handle.turns);
        store.save_run_progress(run).await?;

        let peer_receipt = collect_peer_receipt(
            repo,
            &peer_wt,
            contract,
            &peer_unit,
            adapter_kind.as_str(),
            &handle,
            base_sha,
            &peer.allowed_globs,
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
                    "work_unit_id": peer_unit.id,
                    "commit_sha": peer_receipt.commit_sha,
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
    let test = run_test_command(inner_wt, &contract.test_command, contract.test_timeout_secs)?;
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
        WorkerAdapterKind::Cmd => {
            CmdAdapter::new(
                cfg.file.worker.cmd.clone(),
                cfg.file.worker.cmd_args.clone(),
            )
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
