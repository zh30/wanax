//! `wanax watch <run_id>` — poll a run until it reaches a terminal state.
//!
//! `status` is for "now", `notify_cmd` is for hooks, `watch` is for the
//! human staring at a terminal. Polls the store, prints state changes
//! and last-event timestamps, exits when the run is terminal (exit code
//! mirrors run health: 0 accepted, 1 anything else).

use std::path::PathBuf;
use std::time::{Duration, Instant};
use wanax_core::error::WanaxError;
use wanax_core::types::RunState;
use wanax_core::Store;

pub struct WatchOpts {
    pub run_id: String,
    /// Poll interval seconds.
    pub interval_secs: u64,
    /// Give up after this many seconds (0 = never).
    pub timeout_secs: u64,
    pub data_dir: PathBuf,
}

pub async fn run(opts: WatchOpts) -> Result<(), WanaxError> {
    let store = Store::open(&opts.data_dir.join("wanax.db")).await?;
    let start = Instant::now();
    let mut last_state: Option<RunState> = None;
    loop {
        let run = store.get_run(&opts.run_id).await?;
        if last_state != Some(run.state) {
            let spent = wanax_core::money::format_usd(run.spent_usd_micros);
            println!("[{:.0}s] {} ${}", start.elapsed().as_secs(), run.state.as_str(), spent);
            last_state = Some(run.state);
        }
        if run.state.is_terminal() {
            return if run.state == RunState::Accepted {
                Ok(())
            } else {
                Err(wanax_core::error::WanaxError::new(
                    wanax_core::error::ErrorCode::Rejected,
                    format!("run {} reached {}", run.id, run.state.as_str()),
                ))
            };
        }
        if opts.timeout_secs > 0 && start.elapsed().as_secs() >= opts.timeout_secs {
            eprintln!("watch timed out after {}s (state={})", opts.timeout_secs, run.state.as_str());
            return Err(wanax_core::error::WanaxError::from_code(
                wanax_core::error::ErrorCode::WorkerTimeout,
            ));
        }
        tokio::time::sleep(Duration::from_secs(opts.interval_secs.max(1))).await;
    }
}
