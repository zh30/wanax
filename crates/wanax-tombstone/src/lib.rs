mod canonical;

use canonical::canonical_json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use wanax_core::error::{ErrorCode, WanaxError};
use wanax_core::ids::new_id;
use wanax_core::redact::redact;
use wanax_core::timeutil::now_rfc3339;

pub use canonical::canonical_json as to_canonical_json;

pub const SCHEMA_VERSION: &str = "1.0.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    Human,
    Commander,
    Master,
    Goal,
    Peer,
    Verifier,
    System,
}

impl Actor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Commander => "commander",
            Self::Master => "master",
            Self::Goal => "goal",
            Self::Peer => "peer",
            Self::Verifier => "verifier",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    RunStarted,
    UnitDispatched,
    ReceiptSubmitted,
    OuterTestStarted,
    OuterTestFinished,
    Verdict,
    BudgetTick,
    StateChanged,
    Error,
    Cancelled,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunStarted => "run_started",
            Self::UnitDispatched => "unit_dispatched",
            Self::ReceiptSubmitted => "receipt_submitted",
            Self::OuterTestStarted => "outer_test_started",
            Self::OuterTestFinished => "outer_test_finished",
            Self::Verdict => "verdict",
            Self::BudgetTick => "budget_tick",
            Self::StateChanged => "state_changed",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TombstoneEvent {
    pub id: String,
    pub at: String,
    pub actor: Actor,
    pub kind: EventKind,
    pub payload: Value,
    pub payload_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TombstoneEnvelope {
    pub schema_version: String,
    pub run_id: String,
    pub contract_sha256: String,
    pub current_state: String,
    pub events: Vec<TombstoneEvent>,
}

impl TombstoneEnvelope {
    pub fn new(run_id: String, contract_sha256: String, current_state: String) -> Self {
        Self {
            schema_version: SCHEMA_VERSION.to_string(),
            run_id,
            contract_sha256,
            current_state,
            events: Vec::new(),
        }
    }
}

pub fn payload_sha256(payload: &Value) -> String {
    let canon = canonical_json(payload);
    let mut hasher = Sha256::new();
    hasher.update(canon.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn make_event(actor: Actor, kind: EventKind, payload: Value) -> TombstoneEvent {
    let payload_sha256 = payload_sha256(&payload);
    TombstoneEvent {
        id: new_id(),
        at: now_rfc3339(),
        actor,
        kind,
        payload,
        payload_sha256,
    }
}

pub fn run_dir(repo_root: &Path, run_id: &str) -> PathBuf {
    repo_root.join(".wanax").join("runs").join(run_id)
}

pub fn envelope_path(repo_root: &Path, run_id: &str) -> PathBuf {
    run_dir(repo_root, run_id).join("envelope.json")
}

pub fn markdown_path(repo_root: &Path, run_id: &str) -> PathBuf {
    run_dir(repo_root, run_id).join("TOMBSTONE.md")
}

pub fn load_envelope(repo_root: &Path, run_id: &str) -> Result<TombstoneEnvelope, WanaxError> {
    let path = envelope_path(repo_root, run_id);
    let raw = fs::read(&path).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    serde_json::from_slice(&raw).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))
}

pub fn render_markdown(env: &TombstoneEnvelope) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Tombstone {}\n\nschema {} · contract `{}` · state **{}**\n\n",
        env.run_id, env.schema_version, env.contract_sha256, env.current_state
    ));
    for ev in &env.events {
        out.push_str(&format!(
            "## {} · {} · {}\n\n- actor: `{}`\n- id: `{}`\n- payload_sha256: `{}`\n\n```json\n{}\n```\n\n",
            ev.at,
            ev.kind.as_str(),
            ev.actor.as_str(),
            ev.actor.as_str(),
            ev.id,
            ev.payload_sha256,
            serde_json::to_string_pretty(&ev.payload).unwrap_or_else(|_| "{}".into())
        ));
    }
    redact(&out)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), WanaxError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
        f.write_all(bytes)
            .and_then(|_| f.flush())
            .and_then(|_| f.sync_all())
            .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    }
    fs::rename(&tmp, path).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    Ok(())
}

pub fn persist_envelope(repo_root: &Path, env: &TombstoneEnvelope) -> Result<(), WanaxError> {
    let json =
        serde_json::to_vec_pretty(env).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    let json = redact(&String::from_utf8_lossy(&json)).into_bytes();
    let env_path = envelope_path(repo_root, &env.run_id);
    atomic_write(&env_path, &json)?;
    let md = render_markdown(env);
    atomic_write(&markdown_path(repo_root, &env.run_id), md.as_bytes())?;
    Ok(())
}

pub fn append_event(
    repo_root: &Path,
    run_id: &str,
    current_state: &str,
    event: TombstoneEvent,
) -> Result<TombstoneEnvelope, WanaxError> {
    let mut env = load_envelope(repo_root, run_id)?;
    env.current_state = current_state.to_string();
    env.events.push(event);
    persist_envelope(repo_root, &env)?;
    Ok(env)
}

pub fn init_envelope(
    repo_root: &Path,
    run_id: &str,
    contract_sha256: &str,
    current_state: &str,
    first: TombstoneEvent,
) -> Result<TombstoneEnvelope, WanaxError> {
    let mut env = TombstoneEnvelope::new(
        run_id.to_string(),
        contract_sha256.to_string(),
        current_state.to_string(),
    );
    env.events.push(first);
    persist_envelope(repo_root, &env)?;
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn envelope_roundtrip_restores_events() {
        let tmp = TempDir::new().unwrap();
        let ev = make_event(
            Actor::System,
            EventKind::RunStarted,
            json!({"base_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}),
        );
        init_envelope(
            tmp.path(),
            "wx_01AAAAAAAAAAAAAAAAAAAAAAAA",
            &"ab".repeat(32),
            "dispatched",
            ev,
        )
        .unwrap();
        let loaded = load_envelope(tmp.path(), "wx_01AAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        assert_eq!(loaded.schema_version, "1.0.0");
        assert_eq!(loaded.events.len(), 1);
        assert_eq!(loaded.events[0].kind, EventKind::RunStarted);
        assert_eq!(
            loaded.events[0].payload_sha256,
            payload_sha256(&loaded.events[0].payload)
        );
        let md =
            fs::read_to_string(markdown_path(tmp.path(), "wx_01AAAAAAAAAAAAAAAAAAAAAAAA")).unwrap();
        assert!(md.contains("run_started"));
    }
}
