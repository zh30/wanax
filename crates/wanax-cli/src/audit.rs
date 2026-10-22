//! `wanax audit <run_id>` — mechanical integrity check on the tombstone.
//!
//! The envelope claims to be the source of truth; until now nothing ever
//! verified that claim. `audit` recomputes every event's `payload_sha256`,
//! checks sequence monotonicity and id uniqueness, validates the envelope's
//! structural invariants (starts with run_started, current_state parses,
//! terminal states have a matching verdict), and cross-checks the DB row
//! and the replayed unit set. Any issue → exit E_AUDIT.

use crate::replay;
use std::collections::HashSet;
use std::path::PathBuf;
use wanax_core::error::{ErrorCode, WanaxError};
use wanax_core::types::RunState;
use wanax_core::Store;
use wanax_tombstone::{payload_sha256, EventKind, TombstoneEnvelope, SCHEMA_VERSION};

#[derive(Debug)]
struct Issue {
    /// Severity: "fail" aborts the audit result; "warn" reports only.
    fail: bool,
    what: String,
}

/// Pure envelope checks — no filesystem, no DB.
fn verify_envelope(env: &TombstoneEnvelope) -> Vec<Issue> {
    let mut issues = Vec::new();
    if env.schema_version != SCHEMA_VERSION {
        issues.push(Issue {
            fail: false,
            what: format!(
                "schema_version {} != {}",
                env.schema_version, SCHEMA_VERSION
            ),
        });
    }
    if env.events.is_empty() {
        issues.push(Issue {
            fail: true,
            what: "envelope has no events".into(),
        });
        return issues;
    }
    if env.events[0].kind != EventKind::RunStarted {
        issues.push(Issue {
            fail: true,
            what: format!(
                "first event is {}, not run_started",
                env.events[0].kind.as_str()
            ),
        });
    }
    let mut ids = HashSet::new();
    let mut last_at: Option<String> = None;
    for (i, ev) in env.events.iter().enumerate() {
        let want = payload_sha256(&ev.payload);
        if want != ev.payload_sha256 {
            issues.push(Issue {
                fail: true,
                what: format!("event {i} {}: payload_sha256 mismatch", ev.kind.as_str()),
            });
        }
        if !ids.insert(&ev.id) {
            issues.push(Issue {
                fail: true,
                what: format!("event {i}: duplicate id {}", ev.id),
            });
        }
        if let Some(prev) = &last_at {
            if ev.at < *prev {
                issues.push(Issue {
                    fail: false,
                    what: format!("event {i} {}: timestamp moved backwards", ev.kind.as_str()),
                });
            }
        }
        last_at = Some(ev.at.clone());
    }
    // current_state must be a real run state, and a terminal envelope needs
    // at least one terminal-evidencing event.
    let st = RunState::parse(&env.current_state);
    match st {
        None => issues.push(Issue {
            fail: true,
            what: format!(
                "current_state {:?} is not a valid run state",
                env.current_state
            ),
        }),
        Some(s) if s.is_terminal() => {
            let terminal_ev = env.events.iter().any(|e| {
                matches!(e.kind, EventKind::Cancelled)
                    || (e.kind == EventKind::Verdict
                        && e.payload
                            .get("decision")
                            .and_then(|d| d.as_str())
                            .is_some_and(|d| matches!(d, "accept" | "reject")))
            });
            if !terminal_ev {
                issues.push(Issue {
                    fail: true,
                    what: format!(
                        "current_state={} is terminal but no accept/reject/cancel event exists",
                        env.current_state
                    ),
                });
            }
        }
        _ => {}
    }
    issues
}

pub struct AuditOpts {
    pub run_id: String,
    pub data_dir: PathBuf,
}

pub async fn run(opts: AuditOpts) -> Result<(), WanaxError> {
    let store = Store::open(&opts.data_dir.join("wanax.db")).await?;
    let run = store.get_run(&opts.run_id).await?;
    let repo = PathBuf::from(&run.repo_root);
    let env = wanax_tombstone::load_envelope(&repo, &opts.run_id)?;

    let mut issues = verify_envelope(&env);

    // Cross-check the DB row — tombstone is truth, divergence = tamper or
    // corruption either way.
    if env.contract_sha256 != run.contract_sha256 {
        issues.push(Issue {
            fail: true,
            what: format!(
                "contract_sha256 mismatch: envelope={} db={}",
                &env.contract_sha256[..env.contract_sha256.len().min(12)],
                &run.contract_sha256[..run.contract_sha256.len().min(12)]
            ),
        });
    }
    if env.current_state != run.state.as_str() {
        issues.push(Issue {
            fail: false,
            what: format!(
                "state divergence: envelope={} db={}",
                env.current_state,
                run.state.as_str()
            ),
        });
    }
    // Replayed unit count should cover the DB rows (replay may skip units
    // that never dispatched — it is a superset of in-flight work, DB may
    // hold units the envelope never saw on very old runs → warn only).
    let snap = replay::envelope_snapshot(&repo, &opts.run_id);
    let db_units = store
        .work_units_for_run(&opts.run_id)
        .await
        .unwrap_or_default();
    if let Some(snap) = &snap {
        let db_ids: HashSet<&str> = db_units.iter().map(|u| u.id.as_str()).collect();
        for u in &snap.units {
            if !db_ids.contains(u.id.as_str()) {
                issues.push(Issue {
                    fail: false,
                    what: format!(
                        "unit {} present in envelope replay but missing from DB",
                        u.id
                    ),
                });
            }
        }
    }

    println!("audit {}", opts.run_id);
    let mut fails = 0usize;
    let mut warns = 0usize;
    for i in &issues {
        if i.fail {
            fails += 1;
            println!("  FAIL {}", i.what);
        } else {
            warns += 1;
            println!("  warn {}", i.what);
        }
    }
    if fails == 0 {
        println!(
            "integrity: OK — {} event(s), {} unit(s) replayed{}",
            env.events.len(),
            snap.map(|s| s.units.len()).unwrap_or(0),
            if warns > 0 {
                format!(", {warns} warning(s)")
            } else {
                String::new()
            }
        );
        return Ok(());
    }
    Err(WanaxError::new(
        ErrorCode::Audit,
        format!("{fails} integrity failure(s), {warns} warning(s)"),
    ))
}
