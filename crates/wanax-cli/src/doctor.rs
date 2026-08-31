use std::path::Path;
use wanax_core::config::load_merged_config;
use wanax_core::error::{ErrorCode, WanaxError};
use wanax_core::lock::inspect_lock;
use wanax_core::{clear_stale_lock, Store};
use wanax_git::is_git_repo;

pub async fn run(fix_lock: bool, strict: bool, data_dir: &Path) -> Result<(), WanaxError> {
    let cwd = std::env::current_dir().map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    let git_ok = is_git_repo(&cwd) && which::which("git").is_ok();
    println!("git: {}", if git_ok { "ok" } else { "missing" });

    let cfg = if cwd.join(".wanax").join("config.toml").is_file() {
        load_merged_config(&cwd, data_dir).ok()
    } else {
        None
    };
    let adapter_name = cfg
        .as_ref()
        .map(|c| c.file.worker.adapter.as_str())
        .unwrap_or("octoscode");
    let adapter_bin = cfg
        .as_ref()
        .map(|c| c.file.worker.octoscode_bin.as_str())
        .unwrap_or("octoscode");
    if adapter_name == "fake" {
        println!("adapter fake: ok");
    } else {
        match which::which(adapter_bin) {
            Ok(_) => {
                let octo = wanax_worker::OctoscodeAdapter::new(adapter_bin);
                match octo.has_yolo_flag() {
                    Ok(true) => println!("adapter {adapter_bin}: ok"),
                    Ok(false) => println!(
                        "adapter {adapter_bin}: [NEEDS CLARIFICATION] --yolo flag not found"
                    ),
                    Err(_) => println!("adapter {adapter_bin}: missing"),
                }
            }
            Err(_) => println!("adapter {adapter_bin}: missing"),
        }
    }

    let cmd_key = std::env::var("WANAX_COMMANDER_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .is_some();
    let inner_key = std::env::var("WANAX_INNER_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .is_some();
    println!(
        "WANAX_COMMANDER_API_KEY: {}",
        if cmd_key { "present" } else { "missing" }
    );
    println!(
        "WANAX_INNER_API_KEY: {}",
        if inner_key { "present" } else { "missing" }
    );

    match inspect_lock(&cwd) {
        Ok(None) => println!("lock: none"),
        Ok(Some((info, true))) => {
            println!("lock: held run={} pid={}", info.run_id, info.pid);
        }
        Ok(Some((info, false))) => {
            println!("lock: stale lock pid={} run={}", info.pid, info.run_id);
            if fix_lock {
                let cleared = clear_stale_lock(&cwd)?;
                if let Ok(store) = Store::open(&data_dir.join("wanax.db")).await {
                    if let Ok(mut run) = store.get_run(&cleared.run_id).await {
                        if !run.state.is_terminal() {
                            let _ = store
                                .set_state(
                                    &mut run,
                                    wanax_core::RunState::Failed,
                                    Some(format!("stale lock pid={}", cleared.pid)),
                                )
                                .await;
                        }
                    }
                }
                println!("lock: cleared");
            }
        }
        Err(e) => println!("lock: error {}", e.message),
    }

    let probe = cwd.join(".wanax").join(".doctor_write");
    let writable = (|| {
        std::fs::create_dir_all(cwd.join(".wanax"))?;
        std::fs::write(&probe, b"ok")?;
        std::fs::remove_file(&probe)?;
        std::io::Result::Ok(())
    })()
    .is_ok();
    println!(
        "disk: {}",
        if writable { "writable" } else { "not writable" }
    );

    if strict && !cmd_key {
        return Err(WanaxError::from_code(ErrorCode::MissingApiKey));
    }
    Ok(())
}
