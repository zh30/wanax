use std::path::Path;
use wanax_core::config::load_merged_config;
use wanax_core::contract::parse_contract_file;
use wanax_core::error::{ErrorCode, WanaxError};
use wanax_core::lock::inspect_locks;
use wanax_core::{clear_stale_lock, Store};
use wanax_git::is_git_repo;
use wanax_verify::allowed_globs_cover_binding_tests;

pub async fn run(fix_lock: bool, strict: bool, data_dir: &Path) -> Result<(), WanaxError> {
    let cwd = std::env::current_dir().map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    let git_ok = is_git_repo(&cwd) && which::which("git").is_ok();
    println!(
        "{}",
        if git_ok {
            crate::i18n::t("git_ok")
        } else {
            crate::i18n::t("git_missing")
        }
    );

    let cfg = if cwd.join(".wanax").join("config.toml").is_file() {
        load_merged_config(&cwd, data_dir).ok()
    } else {
        None
    };
    let adapter_name = cfg
        .as_ref()
        .map(|c| c.file.worker.adapter.as_str())
        .unwrap_or("octoscode");
    match adapter_name {
        "fake" => println!("adapter fake: {}", crate::i18n::t("adapter_ok")),
        "cmd" => {
            let cmd = cfg
                .as_ref()
                .map(|c| c.file.worker.cmd.as_str())
                .unwrap_or("");
            match wanax_worker::resolve_cmd_bin(cmd) {
                Ok(_) => println!("adapter {cmd}: {}", crate::i18n::t("adapter_ok")),
                Err(_) => println!("adapter {cmd}: {}", crate::i18n::t("adapter_missing")),
            }
        }
        "claude" => {
            let bin = cfg
                .as_ref()
                .map(|c| c.file.worker.claude_bin.as_str())
                .unwrap_or("claude");
            match wanax_worker::resolve_cmd_bin(bin) {
                Ok(_) => println!("adapter {bin}: {}", crate::i18n::t("adapter_ok")),
                Err(_) => println!("adapter {bin}: {}", crate::i18n::t("adapter_missing")),
            }
        }
        "codex" => {
            let bin = cfg
                .as_ref()
                .map(|c| c.file.worker.codex_bin.as_str())
                .unwrap_or("codex");
            match wanax_worker::resolve_cmd_bin(bin) {
                Ok(_) => println!("adapter {bin}: {}", crate::i18n::t("adapter_ok")),
                Err(_) => println!("adapter {bin}: {}", crate::i18n::t("adapter_missing")),
            }
        }
        _ => {
            let adapter_bin = cfg
                .as_ref()
                .map(|c| c.file.worker.octoscode_bin.as_str())
                .unwrap_or("octoscode");
            match which::which(adapter_bin) {
                Ok(_) => {
                    let octo = wanax_worker::OctoscodeAdapter::new(adapter_bin);
                    match octo.has_yolo_flag() {
                        Ok(true) => {
                            println!("adapter {adapter_bin}: {}", crate::i18n::t("adapter_ok"))
                        }
                        Ok(false) => println!(
                            "adapter {adapter_bin}: [NEEDS CLARIFICATION] --yolo flag not found"
                        ),
                        Err(_) => println!(
                            "adapter {adapter_bin}: {}",
                            crate::i18n::t("adapter_missing")
                        ),
                    }
                }
                Err(_) => println!(
                    "adapter {adapter_bin}: {}",
                    crate::i18n::t("adapter_missing")
                ),
            }
        }
    }

    if let Some(cfg) = &cfg {
        if wanax_core::commander_is_cli(&cfg.file.commander.provider) {
            let bin = wanax_core::commander_cli_bin(&cfg.file.commander);
            match wanax_worker::resolve_cmd_bin(&bin) {
                Ok(_) => println!("commander {bin}: {}", crate::i18n::t("adapter_ok")),
                Err(_) => println!("commander {bin}: {}", crate::i18n::t("adapter_missing")),
            }
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
        if cmd_key {
            crate::i18n::t("key_present")
        } else {
            crate::i18n::t("key_missing")
        }
    );
    println!(
        "WANAX_INNER_API_KEY: {}",
        if inner_key {
            crate::i18n::t("key_present")
        } else {
            crate::i18n::t("key_missing")
        }
    );

    match inspect_locks(&cwd) {
        Ok(locks) if locks.is_empty() => println!("{}", crate::i18n::t("lock_none")),
        Ok(locks) => {
            let any_stale = locks.iter().any(|(_, alive)| !alive);
            for (info, alive) in &locks {
                if *alive {
                    println!("lock: held run={} pid={}", info.run_id, info.pid);
                } else {
                    println!("lock: stale lock pid={} run={}", info.pid, info.run_id);
                }
            }
            if fix_lock && any_stale {
                let cleared = clear_stale_lock(&cwd)?;
                if let Ok(store) = Store::open(&data_dir.join("wanax.db")).await {
                    for info in &cleared {
                        if let Ok(mut run) = store.get_run(&info.run_id).await {
                            if !run.state.is_terminal() {
                                let _ = store
                                    .set_state(
                                        &mut run,
                                        wanax_core::RunState::Failed,
                                        Some(format!("stale lock pid={}", info.pid)),
                                    )
                                    .await;
                            }
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
        "{}",
        if writable {
            crate::i18n::t("disk_writable")
        } else {
            crate::i18n::t("disk_not_writable")
        }
    );

    let tests_writable = scan_writable_test_contracts(&cwd);
    if tests_writable.is_empty() {
        println!("{}", crate::i18n::t("contracts_ok"));
    } else {
        for rel in &tests_writable {
            println!(
                "WARN {} {} ({rel})",
                ErrorCode::ContractTestsWritable.as_str(),
                ErrorCode::ContractTestsWritable.default_message()
            );
        }
    }

    if let Some(cfg) = &cfg {
        if cfg
            .file
            .verify
            .plugins
            .iter()
            .any(|p| p == "agent-spec" || p == "agent_spec")
        {
            let bin = if cfg.file.verify.agent_spec_bin.is_empty() {
                "agent-spec"
            } else {
                cfg.file.verify.agent_spec_bin.as_str()
            };
            if which::which(bin).is_ok() || std::path::Path::new(bin).is_file() {
                println!("{}", crate::i18n::t("plugin_ok"));
            } else {
                println!("{}", crate::i18n::t("plugin_missing"));
            }
        } else {
            println!("{}", crate::i18n::t("plugin_off"));
        }
    }

    let commander_cli = cfg
        .as_ref()
        .is_some_and(|c| wanax_core::commander_is_cli(&c.file.commander.provider));
    if strict && !cmd_key && !commander_cli {
        return Err(WanaxError::from_code(ErrorCode::MissingApiKey));
    }
    if strict && !tests_writable.is_empty() {
        return Err(WanaxError::from_code(ErrorCode::ContractTestsWritable));
    }
    Ok(())
}

fn scan_writable_test_contracts(cwd: &Path) -> Vec<String> {
    let specs = cwd.join("specs");
    let Ok(rd) = std::fs::read_dir(&specs) else {
        return Vec::new();
    };
    let mut hits = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if !name.ends_with(".contract.md") {
            continue;
        }
        let rel = format!("specs/{name}");
        let Ok(contract) = parse_contract_file(&path, &rel) else {
            continue;
        };
        if allowed_globs_cover_binding_tests(&contract.allowed_globs) {
            hits.push(rel);
        }
    }
    hits.sort();
    hits
}
