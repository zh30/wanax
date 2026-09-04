mod doctor;
mod engine;
mod gh;
mod i18n;
mod init;
mod logging;

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::process::ExitCode;
use wanax_core::config::global_data_dir;
use wanax_core::error::{ErrorCode, WanaxError};
use wanax_core::money::format_usd_4;
use wanax_core::types::WorkerAdapterKind;
use wanax_core::Store;

#[derive(Parser)]
#[command(name = "wanax", version, about = "Wanax — lights-off software factory")]
struct Cli {
    /// Directory for wanax.db (default: ~/.wanax)
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// CLI language: en (default) or zh
    #[arg(long, global = true)]
    lang: Option<String>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, ValueEnum)]
enum AdapterArg {
    Octoscode,
    Fake,
    Cmd,
    Claude,
    Codex,
}

impl From<AdapterArg> for WorkerAdapterKind {
    fn from(v: AdapterArg) -> Self {
        match v {
            AdapterArg::Octoscode => Self::Octoscode,
            AdapterArg::Fake => Self::Fake,
            AdapterArg::Cmd => Self::Cmd,
            AdapterArg::Claude => Self::Claude,
            AdapterArg::Codex => Self::Codex,
        }
    }
}

#[derive(Subcommand)]
enum Commands {
    Init {
        #[arg(long)]
        force: bool,
    },
    Start {
        #[arg(long)]
        contract: PathBuf,
        #[arg(long)]
        allow_dirty: bool,
        #[arg(long)]
        adapter: Option<AdapterArg>,
    },
    Status {
        run_id: Option<String>,
    },
    Cancel {
        run_id: String,
    },
    Resume {
        run_id: Option<String>,
        #[arg(long)]
        allow_dirty: bool,
        #[arg(long)]
        adapter: Option<AdapterArg>,
    },
    Doctor {
        #[arg(long)]
        fix_lock: bool,
        #[arg(long)]
        strict: bool,
    },
    Verdict {
        run_id: String,
    },
    Cost {
        run_id: Option<String>,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{}", err.stderr_line());
            ExitCode::from(u8::try_from(err.exit_code()).unwrap_or(1))
        }
    }
}

async fn run() -> Result<(), WanaxError> {
    let cli = Cli::parse();
    i18n::set_lang(i18n::resolve_lang(cli.lang.as_deref()));
    let data_dir = cli.data_dir.unwrap_or_else(global_data_dir);
    match cli.command {
        Commands::Init { force } => init::run(force),
        Commands::Start {
            contract,
            allow_dirty,
            adapter,
        } => {
            engine::start(engine::StartOpts {
                contract,
                allow_dirty,
                adapter: adapter.map(Into::into),
                data_dir,
            })
            .await
        }
        Commands::Status { run_id } => status(data_dir, run_id).await,
        Commands::Cancel { run_id } => cancel(data_dir, run_id).await,
        Commands::Resume {
            run_id,
            allow_dirty,
            adapter,
        } => {
            engine::resume(engine::ResumeOpts {
                run_id,
                allow_dirty,
                adapter: adapter.map(Into::into),
                data_dir,
            })
            .await
        }
        Commands::Doctor { fix_lock, strict } => doctor::run(fix_lock, strict, &data_dir).await,
        Commands::Verdict { run_id } => print_verdict(data_dir, run_id).await,
        Commands::Cost { run_id } => print_cost(data_dir, run_id).await,
    }
}

async fn open_store(data_dir: PathBuf) -> Result<Store, WanaxError> {
    std::fs::create_dir_all(&data_dir).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    Store::open(&data_dir.join("wanax.db")).await
}

async fn status(data_dir: PathBuf, run_id: Option<String>) -> Result<(), WanaxError> {
    let store = open_store(data_dir).await?;
    match run_id {
        None => {
            let runs = store.list_runs().await?;
            if runs.is_empty() {
                println!("{}", i18n::t("no_runs"));
                return Ok(());
            }
            if i18n::current_lang() == i18n::Lang::Zh {
                println!(
                    "{:<32} {:<18} {:<8} {:<10} {:<6} 最近事件",
                    "运行", "状态", "单元", "美元", "回合"
                );
            } else {
                println!(
                    "{:<32} {:<18} {:<8} {:<10} {:<6} LastEvent",
                    "Run", "State", "Unit", "USD", "Turns"
                );
            }
            for run in runs {
                let units = store.work_units_for_run(&run.id).await.unwrap_or_default();
                let unit = units.last().map(|u| u.id.as_str()).unwrap_or("-");
                let last = last_event_at(&run.repo_root, &run.id).unwrap_or_else(|| "-".into());
                println!(
                    "{:<32} {:<18} {:<8} {:<10} {:<6} {}",
                    run.id,
                    run.state.as_str(),
                    &unit[unit.len().saturating_sub(8)..],
                    format_usd_4(run.spent_usd_micros),
                    run.spent_inner_turns,
                    last
                );
            }
            Ok(())
        }
        Some(id) => {
            let run = store.get_run(&id).await?;
            let units = store.work_units_for_run(&run.id).await?;
            println!("run_id={}", run.id);
            println!("state={}", run.state.as_str());
            println!("spent_usd={}", format_usd_4(run.spent_usd_micros));
            println!("spent_turns={}", run.spent_inner_turns);
            if let Some(u) = units.last() {
                println!("work_unit={} {}", u.id, u.title);
                println!("rework_count={}", u.rework_count);
                if let Some(vid) = &u.verdict_id {
                    if let Ok(Some(v)) = store.latest_verdict(&u.id).await {
                        println!(
                            "outer_test_exit={} boundary_ok={} decision={}",
                            v.outer_test_exit_code,
                            v.boundary_ok,
                            v.decision.as_str()
                        );
                        let _ = vid;
                    }
                }
            } else {
                println!("work_unit=-");
            }
            if let Some(at) = last_event_at(&run.repo_root, &run.id) {
                println!("last_event={at}");
            }
            if let Some(err) = &run.last_error {
                println!("last_error={err}");
            }
            let contract_path = std::path::Path::new(&run.repo_root).join(
                store
                    .get_contract(&run.contract_id)
                    .await
                    .map(|c| c.path)
                    .unwrap_or_default(),
            );
            if contract_path.is_file() {
                if let Ok(disk) = wanax_core::hashutil::sha256_file(&contract_path) {
                    if disk != run.contract_sha256 {
                        eprintln!(
                            "WARN {} {}",
                            ErrorCode::ContractMutated.as_str(),
                            ErrorCode::ContractMutated.default_message()
                        );
                    }
                }
            }
            Ok(())
        }
    }
}

fn last_event_at(repo_root: &str, run_id: &str) -> Option<String> {
    let env = wanax_tombstone::load_envelope(std::path::Path::new(repo_root), run_id).ok()?;
    env.events.last().map(|e| e.at.clone())
}

async fn print_cost(data_dir: PathBuf, run_id: Option<String>) -> Result<(), WanaxError> {
    let store = open_store(data_dir).await?;
    match run_id {
        None => {
            let runs = store.list_runs().await?;
            if runs.is_empty() {
                println!("{}", i18n::t("no_runs"));
                return Ok(());
            }
            println!("{}", i18n::t("cost_header"));
            for run in runs {
                println!(
                    "{:<32} {:<18} {:<10} {:<22} {:<18} {:<10} {:<6}",
                    run.id,
                    run.state.as_str(),
                    run.worker_adapter.as_str(),
                    run.commander_model,
                    run.inner_model,
                    format_usd_4(run.spent_usd_micros),
                    run.spent_inner_turns
                );
            }
            Ok(())
        }
        Some(id) => {
            let run = store.get_run(&id).await?;
            println!("run_id={}", run.id);
            println!("state={}", run.state.as_str());
            println!("adapter={}", run.worker_adapter.as_str());
            println!("commander_model={}", run.commander_model);
            println!("inner_model={}", run.inner_model);
            println!("spent_usd={}", format_usd_4(run.spent_usd_micros));
            println!("spent_turns={}", run.spent_inner_turns);
            if let Ok(env) = wanax_tombstone::load_envelope(
                std::path::Path::new(&run.repo_root),
                &run.id,
            ) {
                for ev in env
                    .events
                    .iter()
                    .filter(|e| matches!(e.kind, wanax_tombstone::EventKind::BudgetTick))
                {
                    let estimated = ev
                        .payload
                        .get("cost_estimated")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let micros = ev
                        .payload
                        .get("spent_usd_micros")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    print!(
                        "tick at={} spent_usd={} estimated={estimated}",
                        ev.at,
                        format_usd_4(micros)
                    );
                    if let Some(p) = ev.payload.get("prompt_tokens").and_then(|v| v.as_u64()) {
                        print!(" prompt_tokens={p}");
                    }
                    if let Some(c) = ev.payload.get("completion_tokens").and_then(|v| v.as_u64()) {
                        print!(" completion_tokens={c}");
                    }
                    println!();
                }
            }
            Ok(())
        }
    }
}

async fn print_verdict(data_dir: PathBuf, run_id: String) -> Result<(), WanaxError> {
    let store = open_store(data_dir).await?;
    let _run = store.get_run(&run_id).await?;
    let units = store.work_units_for_run(&run_id).await?;
    let Some(unit) = units.last() else {
        println!("{}", i18n::t("no_verdict"));
        return Ok(());
    };
    match store.latest_verdict(&unit.id).await? {
        Some(v) => {
            println!("decision={}", v.decision.as_str());
            println!("reason={}", v.reason);
            println!("outer_test_exit_code={}", v.outer_test_exit_code);
            println!("boundary_ok={}", v.boundary_ok);
            println!("commander_model={}", v.commander_model);
        }
        None => println!("{}", i18n::t("no_verdict")),
    }
    Ok(())
}

async fn cancel(data_dir: PathBuf, run_id: String) -> Result<(), WanaxError> {
    let store = open_store(data_dir).await?;
    let mut run = store.get_run(&run_id).await?;
    if run.state.is_terminal() {
        let path = std::path::Path::new(&run.repo_root);
        let _ = wanax_core::lock::release_run_lock(path, &run.id);
        return Ok(());
    }
    let _ = store
        .set_state(&mut run, wanax_core::RunState::Canceling, None)
        .await;
    let pids: Vec<u32> = [run.worker_pid, run.start_pid]
        .into_iter()
        .flatten()
        .filter_map(|p| u32::try_from(p).ok())
        .collect();
    for pid in &pids {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(wanax_core::CANCEL_TIMEOUT_SECS);
    while std::time::Instant::now() < deadline {
        if pids.iter().all(|p| !wanax_core::pid_alive(*p)) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    for pid in &pids {
        if wanax_core::pid_alive(*pid) {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
    }
    let repo = PathBuf::from(&run.repo_root);
    if let Ok(mut live) = store.get_run(&run_id).await {
        if !live.state.is_terminal() {
            let _ = store
                .set_state(&mut live, wanax_core::RunState::Cancelled, None)
                .await;
            run = live;
        }
    }
    let _ = store
        .set_state(&mut run, wanax_core::RunState::Cancelled, None)
        .await;
    if let Ok(mut env) = wanax_tombstone::load_envelope(&repo, &run_id) {
        env.current_state = "cancelled".into();
        env.events.push(wanax_tombstone::make_event(
            wanax_tombstone::Actor::Human,
            wanax_tombstone::EventKind::Cancelled,
            serde_json::json!({}),
        ));
        let _ = wanax_tombstone::persist_envelope(&repo, &env);
    }
    let _ = wanax_core::lock::release_run_lock(&repo, &run_id);
    Ok(())
}
