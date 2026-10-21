//! `wanax diff <run_id>` — the run's inner diff, on demand.
//!
//! The first question after a run is always "what did it actually
//! change". `export` bundles it for archive; `diff` answers inline:
//! stat by default, `--patch` for the full unified diff.

use std::path::PathBuf;
use wanax_core::error::WanaxError;
use wanax_core::Store;

pub struct DiffOpts {
    pub run_id: String,
    pub patch: bool,
    pub data_dir: PathBuf,
}

pub async fn run(opts: DiffOpts) -> Result<(), WanaxError> {
    let store = Store::open(&opts.data_dir.join("wanax.db")).await?;
    let run = store.get_run(&opts.run_id).await?;
    let repo = PathBuf::from(&run.repo_root);
    let branch = format!("wanax/{}/inner", run.id);
    if opts.patch {
        let out = wanax_git::diff_patch(&repo, &run.base_sha, &branch)?;
        print!("{out}");
    } else {
        let stat = wanax_git::diff_stat(&repo, &run.base_sha, &branch)?;
        println!("{branch} (base {})\n{stat}", &run.base_sha[..7.min(run.base_sha.len())]);
    }
    Ok(())
}
