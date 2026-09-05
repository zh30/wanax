use std::process::Command;
use std::time::Instant;
use tempfile::TempDir;
use wanax_core::types::{
    CompletionCriterion, Contract, FactoryRun, RunState, WorkerAdapterKind,
};
use wanax_core::Store;
use wanax_tombstone::{init_envelope, persist_envelope, Actor, EventKind};

fn wanax() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wanax"))
}

fn p95_micros(samples: &mut [u128]) -> u128 {
    samples.sort_unstable();
    let idx = samples
        .len()
        .saturating_sub(1)
        .min(((samples.len() as f64) * 0.95).ceil() as usize - 1);
    samples[idx]
}

#[tokio::test]
async fn status_p95_under_200ms_with_10k_events() {
    let repo = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let run_id = "wx_01AAAAAAAAAAAAAAAAAAAAAAAA";
    let contract = Contract {
        id: "wx_01BBBBBBBBBBBBBBBBBBBBBBBB".into(),
        path: "specs/nfr.contract.md".into(),
        content_sha256: "ab".repeat(32),
        intent: "nfr".into(),
        decisions: vec!["d".into()],
        allowed_globs: vec!["src/**".into()],
        forbidden_globs: vec![],
        forbidden_rules: vec![],
        completion_criteria: vec![CompletionCriterion {
            id: "CC-01".into(),
            statement: "pass".into(),
            bound_test: None,
            must_have_files: vec![],
        }],
        test_command: "cargo test".into(),
        test_timeout_secs: 30,
        name: Some("nfr".into()),
        agent_spec: None,
    };
    let run = FactoryRun {
        id: run_id.into(),
        repo_root: repo.path().display().to_string(),
        contract_id: contract.id.clone(),
        contract_sha256: contract.content_sha256.clone(),
        state: RunState::Accepted,
        base_sha: "a".repeat(40),
        inner_branch: format!("wanax/{run_id}/inner"),
        outer_branch: format!("wanax/{run_id}/outer"),
        commander_model: "commander".into(),
        inner_model: "inner".into(),
        reviewer_model: None,
        max_usd_micros: 5_000_000,
        max_inner_turns: 40,
        spent_usd_micros: 0,
        spent_inner_turns: 1,
        worker_adapter: WorkerAdapterKind::Fake,
        created_at: "2026-09-05T00:00:00Z".into(),
        updated_at: "2026-09-05T00:00:00Z".into(),
        finished_at: None,
        last_error: None,
        worker_pid: None,
        start_pid: None,
    };
    let store = Store::open(&data.path().join("wanax.db")).await.unwrap();
    store.insert_contract(&contract).await.unwrap();
    store.insert_run(&run).await.unwrap();

    let first = wanax_tombstone::make_event(
        Actor::System,
        EventKind::RunStarted,
        serde_json::json!({"base_sha": run.base_sha}),
    );
    let mut env = init_envelope(
        repo.path(),
        run_id,
        &contract.content_sha256,
        "accepted",
        first,
    )
    .unwrap();
    env.events.reserve(10_000);
    for i in 0..10_000 {
        env.events.push(wanax_tombstone::make_event(
            Actor::System,
            EventKind::StateChanged,
            serde_json::json!({"i": i}),
        ));
    }
    persist_envelope(repo.path(), &env).unwrap();

    let mut samples = Vec::with_capacity(20);
    for _ in 0..20 {
        let t0 = Instant::now();
        let out = wanax()
            .args(["status", run_id, "--data-dir"])
            .arg(data.path())
            .current_dir(repo.path())
            .output()
            .unwrap();
        samples.push(t0.elapsed().as_micros());
        assert!(
            out.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains(run_id), "{stdout}");
    }
    let p95 = p95_micros(&mut samples);
    assert!(
        p95 < 200_000,
        "NFR-1 status p95={p95}µs samples={samples:?}"
    );
}
