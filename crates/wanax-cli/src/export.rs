//! `wanax export <run_id>` — bundle a run's audit evidence into one dir.
//!
//! The audit story ends where it can be *handed over*: envelope +
//! frozen contract + inner diff + replayed state + LEARNINGS in a single
//! `.wanax/exports/<run_id>/` (or `--out`). Attach it to a PR, archive
//! it, or hand it to whoever reviews what the factory did.

use crate::replay;
use serde_json::json;
use std::path::PathBuf;
use wanax_core::error::{ErrorCode, WanaxError};
use wanax_core::Store;
use wanax_git::ref_exists;
use wanax_tombstone::{envelope_path, load_envelope, markdown_path};

pub struct ExportOpts {
    pub run_id: String,
    pub out: Option<PathBuf>,
    pub data_dir: PathBuf,
}

pub async fn run(opts: ExportOpts) -> Result<(), WanaxError> {
    let store = Store::open(&opts.data_dir.join("wanax.db")).await?;
    let run = store.get_run(&opts.run_id).await?;
    let repo = PathBuf::from(&run.repo_root);
    let out_dir = opts
        .out
        .unwrap_or_else(|| repo.join(".wanax").join("exports").join(&opts.run_id));
    std::fs::create_dir_all(&out_dir).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;

    let mut files: Vec<String> = Vec::new();
    let copy = |src: &std::path::Path, name: &str, files: &mut Vec<String>| {
        if src.is_file() && std::fs::copy(src, out_dir.join(name)).is_ok() {
            files.push(name.to_string());
        }
    };

    // Tombstone pair — the audit record itself.
    copy(
        &envelope_path(&repo, &opts.run_id),
        "envelope.json",
        &mut files,
    );
    copy(
        &markdown_path(&repo, &opts.run_id),
        "TOMBSTONE.md",
        &mut files,
    );

    // Frozen contract (DB row knows its repo-relative path).
    if let Ok(c) = store.get_contract(&run.contract_id).await {
        let src = repo.join(&c.path);
        copy(&src, "contract.md", &mut files);
    }

    // Replay — what the envelope reconstructs, machine-readable.
    if let Some(snap) = replay::envelope_snapshot(&repo, &opts.run_id) {
        if let Ok(s) = serde_json::to_string_pretty(&snap) {
            if std::fs::write(out_dir.join("replay.json"), s).is_ok() {
                files.push("replay.json".into());
            }
        }
    }

    // The actual product: diff of the inner branch against the base.
    let mut inner_included = false;
    if ref_exists(&repo, &format!("wanax/{}/inner", run.id)) {
        if let Ok(diff) =
            wanax_git::diff_patch(&repo, &run.base_sha, &format!("wanax/{}/inner", run.id))
        {
            if std::fs::write(out_dir.join("inner.diff"), &diff).is_ok() {
                files.push("inner.diff".into());
                inner_included = true;
            }
        }
    }

    // Cross-run memory — the context the run operated under.
    copy(
        &repo.join(".wanax/LEARNINGS.md"),
        "LEARNINGS.md",
        &mut files,
    );

    let env = load_envelope(&repo, &opts.run_id).ok();
    let verdicts: Vec<String> = env
        .as_ref()
        .map(|e| {
            e.events
                .iter()
                .filter(|ev| ev.kind.as_str() == "verdict")
                .filter_map(|ev| {
                    ev.payload
                        .get("decision")
                        .and_then(|d| d.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    let manifest = json!({
        "run_id": run.id,
        "exported_at": wanax_core::timeutil::now_rfc3339(),
        "state": run.state.as_str(),
        "contract_sha256": run.contract_sha256,
        "base_sha": run.base_sha,
        "adapter": run.worker_adapter.as_str(),
        "commander_model": run.commander_model,
        "spent_usd_micros": run.spent_usd_micros,
        "events": env.as_ref().map(|e| e.events.len()).unwrap_or(0),
        "verdicts": verdicts,
        "inner_diff": inner_included,
        "files": files,
    });
    std::fs::write(
        out_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap_or_default(),
    )
    .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;

    println!("exported {}", out_dir.display());
    println!(
        "manifest: {} files, state={}",
        files.len() + 1,
        run.state.as_str()
    );
    Ok(())
}
