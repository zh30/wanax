//! Rebuild resumable run state by replaying the tombstone envelope.
//!
//! The envelope under `.wanax/runs/<id>/` is the audit source of truth; the
//! work_units table is a cache. `resume` prefers replayed state whenever the
//! envelope carries full unit evidence (unit_seeded / extended
//! unit_dispatched / receipt_submitted / verdict payloads) and heals
//! diverged-or-missing DB rows from it. Pre-change envelopes simply lack the
//! fields and the caller falls back to the DB path unchanged.

use std::collections::HashMap;
use std::path::Path;
use wanax_core::types::{AssigneeRole, Receipt, WorkUnit, WorkUnitState};
use wanax_tombstone::{load_envelope, EventKind};

/// The receipt fields replay needs to resume an interrupted outer review.
/// Bulk text (excerpt/diffstat) still comes from the DB row when present.
#[derive(Debug, Clone)]
pub struct ReceiptStub {
    pub receipt_id: String,
    pub work_unit_id: String,
    pub commit_sha: String,
    pub changed_files: Vec<String>,
    pub test_exit_code: i32,
    pub claimed_pass: bool,
}

#[derive(Debug, Default)]
pub struct ReplayOutcome {
    /// All units in dispatch order, with replayed state/rework counts.
    pub units: Vec<WorkUnit>,
    /// Receipt stubs keyed by unit id.
    pub receipts: HashMap<String, ReceiptStub>,
    /// (unit_id, receipt_id) whose verdict never arrived — resume target.
    pub pending: Option<(String, String)>,
}

fn blank_unit(run_id: &str) -> WorkUnit {
    WorkUnit {
        id: String::new(),
        run_id: run_id.to_string(),
        seq: 0,
        title: String::new(),
        instruction: String::new(),
        state: WorkUnitState::Queued,
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
    }
}

fn apply_unit_fields(u: &mut WorkUnit, p: &serde_json::Value) {
    if let Some(v) = p["work_unit_id"].as_str() {
        u.id = v.to_string();
    }
    if let Some(v) = p["seq"].as_u64() {
        u.seq = v as u32;
    }
    if let Some(v) = p["title"].as_str() {
        u.title = v.to_string();
    }
    if let Some(v) = p["instruction"].as_str() {
        u.instruction = v.to_string();
    }
    if let Some(v) = p["role"].as_str().and_then(AssigneeRole::parse) {
        u.assignee_role = v;
    }
    if let Some(v) = p["local_key"].as_str() {
        u.local_key = Some(v.to_string());
    }
    if let Some(v) = p["allowed_globs"].as_array() {
        u.allowed_globs =
            Some(v.iter().filter_map(|x| x.as_str().map(String::from)).collect());
    }
    if let Some(v) = p["depends_on"].as_array() {
        u.depends_on = v.iter().filter_map(|x| x.as_str().map(String::from)).collect();
    }
    if let Some(v) = p["test_command"].as_str() {
        u.test_command = Some(v.to_string());
    }
    if let Some(v) = p["parent_id"].as_str() {
        u.parent_id = Some(v.to_string());
    }
}

fn upsert(
    out: &mut ReplayOutcome,
    by_id: &mut HashMap<String, usize>,
    run_id: &str,
    p: &serde_json::Value,
    state: WorkUnitState,
) -> Option<usize> {
    let id = p["work_unit_id"].as_str()?;
    let idx = if let Some(&i) = by_id.get(id) {
        i
    } else {
        let mut u = blank_unit(run_id);
        u.id = id.to_string();
        out.units.push(u);
        by_id.insert(id.to_string(), out.units.len() - 1);
        out.units.len() - 1
    };
    let u = &mut out.units[idx];
    apply_unit_fields(u, p);
    u.state = state;
    Some(idx)
}

fn apply_verdict_to(
    out: &mut ReplayOutcome,
    by_id: &HashMap<String, usize>,
    id: &str,
    decision: &str,
) {
    if let Some(&i) = by_id.get(id) {
        let u = &mut out.units[i];
        match decision {
            "accept" => u.state = WorkUnitState::Accepted,
            "reject" => u.state = WorkUnitState::Rejected,
            "rework" => {
                u.state = WorkUnitState::Queued;
                u.rework_count += 1;
            }
            _ => {}
        }
    }
}

pub fn envelope_snapshot(repo: &Path, run_id: &str) -> Option<ReplayOutcome> {
    let env = load_envelope(repo, run_id).ok()?;
    let mut out = ReplayOutcome::default();
    let mut by_id: HashMap<String, usize> = HashMap::new();
    // unit_dispatched "covers" (peer master → dag members) for wave verdicts.
    let mut covers: HashMap<String, Vec<String>> = HashMap::new();
    let mut last_dispatched: Option<String> = None;
    let mut last_receipt_unit: Option<String> = None;
    let mut saw_units = false;

    for e in &env.events {
        match e.kind {
            EventKind::UnitSeeded => {
                if upsert(&mut out, &mut by_id, run_id, &e.payload, WorkUnitState::Queued)
                    .is_some()
                {
                    saw_units = true;
                }
            }
            EventKind::UnitDispatched => {
                if let Some(idx) = upsert(
                    &mut out,
                    &mut by_id,
                    run_id,
                    &e.payload,
                    WorkUnitState::Implementing,
                ) {
                    saw_units = true;
                    let id = out.units[idx].id.clone();
                    last_dispatched = Some(id.clone());
                    if let Some(c) = e.payload["covers"].as_array() {
                        covers.insert(
                            id,
                            c.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect(),
                        );
                    }
                }
            }
            EventKind::ReceiptSubmitted => {
                let p = &e.payload;
                let unit_id = p["work_unit_id"]
                    .as_str()
                    .map(String::from)
                    .or_else(|| last_dispatched.clone());
                let Some(unit_id) = unit_id else { continue };
                last_receipt_unit = Some(unit_id.clone());
                let rid = p["receipt_id"].as_str().unwrap_or_default().to_string();
                let stub = ReceiptStub {
                    receipt_id: rid.clone(),
                    work_unit_id: unit_id.clone(),
                    commit_sha: p["commit_sha"].as_str().unwrap_or_default().to_string(),
                    changed_files: p["changed_files"]
                        .as_array()
                        .map(|a| {
                            a.iter().filter_map(|x| x.as_str().map(String::from)).collect()
                        })
                        .unwrap_or_default(),
                    test_exit_code: p["test_exit_code"].as_i64().unwrap_or(0) as i32,
                    claimed_pass: p["claimed_pass"].as_bool().unwrap_or(false),
                };
                if let Some(&i) = by_id.get(&unit_id) {
                    let u = &mut out.units[i];
                    u.state = WorkUnitState::ReceiptReady;
                    u.receipt_id = (!rid.is_empty()).then_some(rid);
                    u.inner_commit_sha = (!stub.commit_sha.is_empty())
                        .then(|| stub.commit_sha.clone());
                }
                out.receipts.insert(unit_id.clone(), stub);
            }
            EventKind::Verdict => {
                let p = &e.payload;
                let target = p["work_unit_id"]
                    .as_str()
                    .map(String::from)
                    .or_else(|| last_receipt_unit.clone());
                let Some(target) = target else { continue };
                let covered = covers.get(&target).cloned().unwrap_or_default();
                let decision = p["decision"].as_str().unwrap_or("");
                apply_verdict_to(&mut out, &by_id, &target, decision);
                for id in covered {
                    apply_verdict_to(&mut out, &by_id, &id, decision);
                }
            }
            _ => {}
        }
    }

    if !saw_units {
        return None;
    }
    // Pending = the latest receipt whose unit never reached a verdict state.
    for u in out.units.iter().rev() {
        if u.state == WorkUnitState::ReceiptReady {
            if let (Some(rid), Some(_)) = (&u.receipt_id, out.receipts.get(&u.id)) {
                out.pending = Some((u.id.clone(), rid.clone()));
                break;
            }
        }
    }
    Some(out)
}

/// True when the DB unit set disagrees with the replayed set — divergence
/// means someone/something touched the cache; the envelope wins.
pub fn units_diverged(db: &[WorkUnit], replayed: &[WorkUnit]) -> bool {
    if db.len() != replayed.len() {
        return true;
    }
    let by_id: HashMap<&str, &WorkUnit> =
        db.iter().map(|u| (u.id.as_str(), u)).collect();
    replayed.iter().any(|s| match by_id.get(s.id.as_str()) {
        None => true,
        Some(d) => {
            d.state != s.state
                || d.rework_count != s.rework_count
                || d.receipt_id != s.receipt_id
                || d.title != s.title
        }
    })
}

/// Synthesize a Receipt from a stub when the DB row is gone — the fields the
/// accept gates consume (commit, changed files, exit code) all live in the
/// envelope; the outer retest re-executes anyway.
pub fn synthesize_receipt(stub: &ReceiptStub, test_command: &str, adapter: &str) -> Receipt {
    Receipt {
        id: stub.receipt_id.clone(),
        work_unit_id: stub.work_unit_id.clone(),
        changed_files: stub.changed_files.clone(),
        diffstat: String::new(),
        commit_sha: stub.commit_sha.clone(),
        test_command: test_command.to_string(),
        test_exit_code: stub.test_exit_code,
        test_excerpt: String::new(),
        claimed_pass: stub.claimed_pass,
        duration_ms: 0,
        adapter: adapter.to_string(),
        raw_artifact_path: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wanax_tombstone::{append_event, init_envelope, make_event, Actor, EventKind};

    fn ev(kind: EventKind, payload: serde_json::Value) -> wanax_tombstone::TombstoneEvent {
        make_event(Actor::System, kind, payload)
    }

    #[test]
    fn replay_rebuilds_dag_units_and_states() {
        let dir = tempfile::tempdir().unwrap();
        let rid = "wx_TEST";
        init_envelope(dir.path(), rid, "sha", "dispatched",
            ev(EventKind::RunStarted, json!({}))).unwrap();
        // Seed two dag units.
        append_event(dir.path(), rid, "queued", ev(EventKind::UnitSeeded, json!({
            "work_unit_id": "u1", "seq": 1, "title": "a", "instruction": "ia",
            "role": "goal", "local_key": "u1", "depends_on": [],
            "allowed_globs": ["src/a.rs"]
        }))).unwrap();
        append_event(dir.path(), rid, "queued", ev(EventKind::UnitSeeded, json!({
            "work_unit_id": "u2", "seq": 2, "title": "b", "instruction": "ib",
            "role": "goal", "local_key": "u2", "depends_on": ["u1"],
            "allowed_globs": ["src/b.rs"]
        }))).unwrap();
        // u1 dispatched → receipt → accept verdict.
        append_event(dir.path(), rid, "queued", ev(EventKind::UnitDispatched, json!({
            "work_unit_id": "u1", "title": "a", "instruction": "ia"
        }))).unwrap();
        append_event(dir.path(), rid, "queued", ev(EventKind::ReceiptSubmitted, json!({
            "work_unit_id": "u1", "receipt_id": "r1", "commit_sha": "abc",
            "changed_files": ["src/a.rs"], "test_exit_code": 0, "claimed_pass": true
        }))).unwrap();
        append_event(dir.path(), rid, "queued", ev(EventKind::Verdict, json!({
            "decision": "accept", "work_unit_id": "u1"
        }))).unwrap();
        // u2 dispatched → receipt → rework.
        append_event(dir.path(), rid, "queued", ev(EventKind::UnitDispatched, json!({
            "work_unit_id": "u2", "title": "b", "instruction": "ib"
        }))).unwrap();
        append_event(dir.path(), rid, "queued", ev(EventKind::ReceiptSubmitted, json!({
            "work_unit_id": "u2", "receipt_id": "r2", "commit_sha": "def",
            "changed_files": ["src/b.rs"], "test_exit_code": 1, "claimed_pass": false
        }))).unwrap();
        append_event(dir.path(), rid, "queued", ev(EventKind::Verdict, json!({
            "decision": "rework", "work_unit_id": "u2"
        }))).unwrap();

        let snap = envelope_snapshot(dir.path(), rid).unwrap();
        assert_eq!(snap.units.len(), 2);
        assert_eq!(snap.units[0].state, WorkUnitState::Accepted);
        assert_eq!(snap.units[1].state, WorkUnitState::Queued);
        assert_eq!(snap.units[1].rework_count, 1);
        assert_eq!(snap.units[1].depends_on, vec!["u1".to_string()]);
        assert!(snap.pending.is_none());
    }

    #[test]
    fn replay_pending_outer_when_verdict_missing() {
        let dir = tempfile::tempdir().unwrap();
        let rid = "wx_TEST2";
        init_envelope(dir.path(), rid, "sha", "dispatched",
            ev(EventKind::RunStarted, json!({}))).unwrap();
        append_event(dir.path(), rid, "queued", ev(EventKind::UnitDispatched, json!({
            "work_unit_id": "u1", "title": "a", "instruction": "ia", "seq": 1
        }))).unwrap();
        append_event(dir.path(), rid, "queued", ev(EventKind::ReceiptSubmitted, json!({
            "work_unit_id": "u1", "receipt_id": "r1", "commit_sha": "abc",
            "changed_files": ["src/a.rs"], "test_exit_code": 0, "claimed_pass": true
        }))).unwrap();
        // crash before verdict
        let snap = envelope_snapshot(dir.path(), rid).unwrap();
        assert_eq!(snap.pending, Some(("u1".into(), "r1".into())));
        let receipt = synthesize_receipt(&snap.receipts["u1"], "cargo test", "fake");
        assert_eq!(receipt.commit_sha, "abc");
        assert_eq!(receipt.changed_files, vec!["src/a.rs".to_string()]);
    }

    #[test]
    fn replay_wave_covers_marks_dag_members() {
        let dir = tempfile::tempdir().unwrap();
        let rid = "wx_TEST3";
        init_envelope(dir.path(), rid, "sha", "dispatched",
            ev(EventKind::RunStarted, json!({}))).unwrap();
        for id in ["d1", "d2"] {
            append_event(dir.path(), rid, "queued", ev(EventKind::UnitSeeded, json!({
                "work_unit_id": id, "seq": 1, "title": id, "instruction": "i",
                "local_key": id, "depends_on": []
            }))).unwrap();
        }
        append_event(dir.path(), rid, "queued", ev(EventKind::UnitDispatched, json!({
            "work_unit_id": "m1", "title": "peer-batch (2)", "instruction": "x",
            "mode": "peer_batch", "covers": ["d1", "d2"]
        }))).unwrap();
        append_event(dir.path(), rid, "queued", ev(EventKind::ReceiptSubmitted, json!({
            "work_unit_id": "m1", "receipt_id": "rm", "commit_sha": "sha",
            "changed_files": ["src/a.rs", "src/b.rs"]
        }))).unwrap();
        append_event(dir.path(), rid, "queued", ev(EventKind::Verdict, json!({
            "decision": "accept", "work_unit_id": "m1"
        }))).unwrap();
        let snap = envelope_snapshot(dir.path(), rid).unwrap();
        let d1 = snap.units.iter().find(|u| u.id == "d1").unwrap();
        let d2 = snap.units.iter().find(|u| u.id == "d2").unwrap();
        assert_eq!(d1.state, WorkUnitState::Accepted);
        assert_eq!(d2.state, WorkUnitState::Accepted);
    }
}
