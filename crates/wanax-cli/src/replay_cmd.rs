//! `wanax replay <run_id>` — what does the envelope think, vs the DB.
//!
//! The tombstone is the source of truth and `resume` already replays it
//! internally — but a human had no way to see the reconstruction. This
//! surfaces `envelope_snapshot` plus a field-level reconciliation against
//! the `work_units` cache: divergence is exactly what `resume` would heal.

use crate::replay::{envelope_snapshot, units_diverged};
use serde_json::json;
use std::path::PathBuf;
use wanax_core::error::WanaxError;
use wanax_core::types::WorkUnit;
use wanax_core::Store;

pub struct ReplayCmdOpts {
    pub run_id: String,
    pub json: bool,
    pub data_dir: PathBuf,
}

/// Field-level difference between a DB row and its replayed counterpart.
fn unit_diffs(db: &WorkUnit, rp: &WorkUnit) -> Vec<String> {
    let mut d = Vec::new();
    if db.state != rp.state {
        d.push(format!(
            "state {} -> {}",
            db.state.as_str(),
            rp.state.as_str()
        ));
    }
    if db.rework_count != rp.rework_count {
        d.push(format!("rework {} -> {}", db.rework_count, rp.rework_count));
    }
    if db.seq != rp.seq {
        d.push(format!("seq {} -> {}", db.seq, rp.seq));
    }
    if db.inner_commit_sha != rp.inner_commit_sha {
        d.push("inner_commit_sha differs".to_string());
    }
    d
}

pub async fn run(opts: ReplayCmdOpts) -> Result<(), WanaxError> {
    let store = Store::open(&opts.data_dir.join("wanax.db")).await?;
    let run = store.get_run(&opts.run_id).await?;
    let repo = PathBuf::from(&run.repo_root);
    let db_units = store.work_units_for_run(&opts.run_id).await?;

    let Some(snap) = envelope_snapshot(&repo, &opts.run_id) else {
        println!(
            "replay {opts_id}: envelope has no replayable unit events",
            opts_id = opts.run_id
        );
        println!("(pre-envelope-era run or no units dispatched — DB path is authoritative)");
        return Ok(());
    };

    // Reconciliation: replayed truth vs DB cache.
    let diverged = units_diverged(&db_units, &snap.units);
    let mut diffs: Vec<serde_json::Value> = Vec::new();
    for ru in &snap.units {
        match db_units.iter().find(|d| d.id == ru.id) {
            None => diffs.push(json!({"unit": ru.id, "detail": "missing in DB"})),
            Some(d) => {
                for f in unit_diffs(d, ru) {
                    diffs.push(json!({"unit": ru.id, "detail": f}));
                }
            }
        }
    }
    for du in &db_units {
        if !snap.units.iter().any(|r| r.id == du.id) {
            diffs.push(json!({"unit": du.id, "detail": "in DB but no envelope evidence"}));
        }
    }

    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "run_id": opts.run_id,
                "envelope_units": snap.units.len(),
                "db_units": db_units.len(),
                "diverged": diverged,
                "divergences": diffs,
                "pending_outer_review": snap.pending,
                "units": snap.units,
                "receipts": snap.receipts,
            }))
            .unwrap_or_default()
        );
        return Ok(());
    }

    println!(
        "replay {} — {} unit(s) reconstructed",
        opts.run_id,
        snap.units.len()
    );
    for u in &snap.units {
        let receipt = snap.receipts.get(&u.id);
        let globs = u
            .allowed_globs
            .as_ref()
            .map(|v| v.join(","))
            .unwrap_or_default();
        println!(
            "  #{:<2} {:<14} {:<10} rework={} {}{}{}",
            u.seq,
            u.state.as_str(),
            u.local_key.as_deref().unwrap_or("-"),
            u.rework_count,
            u.title,
            receipt
                .map(|r| format!(
                    "  [receipt {} exit={}]",
                    &r.commit_sha[..7.min(r.commit_sha.len())],
                    r.test_exit_code
                ))
                .unwrap_or_default(),
            if globs.is_empty() {
                String::new()
            } else {
                format!("  globs={globs}")
            },
        );
    }
    if let Some((u, r)) = &snap.pending {
        println!("  pending outer review: unit={u} receipt={r}");
    }
    if diffs.is_empty() {
        println!("divergence: none — DB cache matches the envelope");
    } else {
        println!(
            "divergence: {} — resume would heal these from the envelope:",
            diffs.len()
        );
        for d in &diffs {
            println!("  {} {}", d["unit"], d["detail"]);
        }
    }
    Ok(())
}
