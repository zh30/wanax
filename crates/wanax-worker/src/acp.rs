//! ACP (agent-client-protocol) worker adapter.
//!
//! One NDJSON-RPC transport drives every ACP-capable agent
//! (`claude-code-acp`, `gemini-cli --acp`, codex wrappers, …):
//! `initialize` → `session/new` → `session/prompt`, streaming
//! `session/update` notifications until the prompt response lands.
//! The agent runs inside the same sandbox/env/proxy confinement as every
//! other adapter — ACP changes the wire, not the threat model.

use crate::{spawn_confined, WorkerContext, WorkerHandle};
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use wanax_core::error::{ErrorCode, WanaxError};

pub struct AcpAdapter {
    pub bin: String,
    pub extra_args: Vec<String>,
}

impl AcpAdapter {
    pub fn new(bin: impl Into<String>, extra_args: Vec<String>) -> Self {
        Self {
            bin: bin.into(),
            extra_args,
        }
    }

    pub fn resolve_bin(&self) -> Result<PathBuf, WanaxError> {
        crate::resolve_cmd_bin(&self.bin)
    }
}

pub struct Outcome {
    text: String,
    transcript: String,
    stop_reason: Option<String>,
    usage_in: Option<u64>,
    usage_out: Option<u64>,
    cost_usd_micros: Option<i64>,
}

/// Run the full `initialize → session/new → session/prompt` exchange and
/// return the collected text + metrics. Interactive: we hold stdin/stdout.
pub async fn run_prompt(
    bin: PathBuf,
    args: &[String],
    ctx: &WorkerContext,
) -> Result<Outcome, WanaxError> {
    let mut cmd = spawn_confined(&bin, args, ctx)?;
    let mut child = cmd.spawn().map_err(|e| {
        WanaxError::new(
            ErrorCode::AdapterMissing,
            format!("adapter binary not found: {}: {e}", bin.display()),
        )
    })?;
    let pid = child.id().unwrap_or(0);
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| WanaxError::from_code(ErrorCode::WorkerCrash))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| WanaxError::from_code(ErrorCode::WorkerCrash))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| WanaxError::from_code(ErrorCode::WorkerCrash))?;
    let mut lines = BufReader::new(stdout).lines();
    let err_task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut e = stderr;
        let mut buf = Vec::new();
        let _ = e.read_to_end(&mut buf).await;
        buf
    });

    let mut transcript = String::new();
    let mut next_id = 1u64;
    async fn rpc(
        stdin: &mut tokio::process::ChildStdin,
        transcript: &mut String,
        next_id: &mut u64,
        method: &str,
        params: Value,
    ) -> Result<u64, WanaxError> {
        let id = *next_id;
        *next_id += 1;
        let msg = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let line = msg.to_string() + "\n";
        transcript.push_str(&line);
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| WanaxError::with_detail(ErrorCode::WorkerCrash, e))?;
        Ok(id)
    }

    rpc(
        &mut stdin,
        &mut transcript,
        &mut next_id,
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}},
        }),
    )
    .await?;
    rpc(
        &mut stdin,
        &mut transcript,
        &mut next_id,
        "session/new",
        json!({"cwd": ctx.worktree, "mcpServers": []}),
    )
    .await?;

    let mut out = Outcome {
        text: String::new(),
        transcript: String::new(),
        stop_reason: None,
        usage_in: None,
        usage_out: None,
        cost_usd_micros: None,
    };
    let mut session_id: Option<String> = None;
    let mut prompt_id: Option<u64> = None;
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(u64::from(ctx.timeout_secs));
    // Read until the prompt response lands; permission requests get the
    // first "allow"-ish option — the OS sandbox is the real boundary.
    while out.stop_reason.is_none() {
        let line = match tokio::time::timeout_at(deadline, lines.next_line()).await {
            Ok(Ok(Some(l))) => l,
            Ok(Ok(None)) => break, // agent closed the stream
            Ok(Err(e)) => {
                crate::kill_process_group(pid, "-KILL");
                return Err(WanaxError::with_detail(ErrorCode::WorkerCrash, e));
            }
            Err(_) => {
                crate::kill_process_group(pid, "-KILL");
                return Err(WanaxError::from_code(ErrorCode::WorkerTimeout));
            }
        };
        transcript.push_str(&line);
        transcript.push('\n');
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
            match method {
                "session/update" => {
                    let upd = &msg["params"]["update"];
                    match upd.get("sessionUpdate").and_then(|k| k.as_str()) {
                        Some("agent_message_chunk") | Some("agent_thought_chunk") => {
                            if let Some(t) = upd["content"]["text"].as_str() {
                                if upd.get("sessionUpdate").and_then(|k| k.as_str())
                                    == Some("agent_message_chunk")
                                {
                                    out.text.push_str(t);
                                }
                            }
                        }
                        Some("usage_update") | Some("usage") => {
                            if let Some(u) = upd["usage"]["usedTokens"]
                                .as_u64()
                                .or_else(|| upd["usedTokens"].as_u64())
                            {
                                out.usage_out = Some(u);
                            }
                            if let Some(a) = upd["cost"]["amount"]
                                .as_f64()
                                .or_else(|| upd["usage"]["cost"]["amount"].as_f64())
                            {
                                out.cost_usd_micros = Some((a * 1e6) as i64);
                            }
                        }
                        _ => {}
                    }
                }
                "session/request_permission" => {
                    let options = msg["params"]["options"].as_array();
                    let pick = options
                        .and_then(|os| {
                            os.iter()
                                .find(|o| {
                                    o.get("kind")
                                        .and_then(|k| k.as_str())
                                        .is_some_and(|k| k.contains("allow"))
                                })
                                .or_else(|| os.first())
                        })
                        .and_then(|o| o.get("optionId").or_else(|| o.get("id")))
                        .and_then(|v| v.as_str())
                        .unwrap_or("allow");
                    if let Some(id) = msg.get("id") {
                        let resp = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {"outcome": {"outcome": "selected", "optionId": pick}}
                        });
                        let l = resp.to_string() + "\n";
                        transcript.push_str(&l);
                        let _ = stdin.write_all(l.as_bytes()).await;
                    }
                }
                _ => {
                    // Unknown agent→client request: method-not-found keeps
                    // the agent from hanging on a reply.
                    if let Some(id) = msg.get("id") {
                        let resp = json!({
                            "jsonrpc": "2.0", "id": id,
                            "error": {"code": -32601, "message": "wanax: unsupported method"}
                        });
                        let l = resp.to_string() + "\n";
                        transcript.push_str(&l);
                        let _ = stdin.write_all(l.as_bytes()).await;
                    }
                }
            }
            continue;
        }
        // A response: match against the request chain.
        if let Some(id) = msg.get("id").and_then(|v| v.as_u64()) {
            if let Some(err) = msg.get("error") {
                crate::kill_process_group(pid, "-KILL");
                return Err(WanaxError::new(
                    ErrorCode::WorkerCrash,
                    format!("acp request {id} failed: {err}"),
                ));
            }
            if id == 1 {
                // initialize ack — nothing to extract.
            } else if session_id.is_none() {
                session_id = msg["result"]["sessionId"].as_str().map(str::to_string);
                let sid = session_id.clone().unwrap_or_default();
                let pid_ = rpc(
                    &mut stdin,
                    &mut transcript,
                    &mut next_id,
                    "session/prompt",
                    json!({
                        "sessionId": sid,
                        "prompt": [{"type": "text", "text": ctx.instruction}],
                    }),
                )
                .await?;
                prompt_id = Some(pid_);
            } else if prompt_id == Some(id) {
                out.stop_reason = msg["result"]["stopReason"].as_str().map(str::to_string);
                if let Some(u) = msg["result"]["usage"]["tokensUsed"].as_u64() {
                    out.usage_out = Some(u);
                }
            }
        }
    }
    // Prompt is done — release the session process and reap it.
    crate::kill_process_group(pid, "-TERM");
    let _ = child.wait().await;
    let err_buf = err_task.await.unwrap_or_default();
    out.transcript = format!("{}\n{}", transcript, String::from_utf8_lossy(&err_buf));
    Ok(out)
}

#[async_trait::async_trait]
impl crate::WorkerAdapter for AcpAdapter {
    async fn start(&self, ctx: &WorkerContext) -> Result<WorkerHandle, WanaxError> {
        let bin = self.resolve_bin()?;
        let started = std::time::Instant::now();
        let out = run_prompt(bin, &self.extra_args, ctx).await?;
        Ok(WorkerHandle {
            pid: 0,
            claimed_pass: out.stop_reason.is_some(),
            test_exit_code: 0,
            test_excerpt: {
                let mut t = out.text.clone();
                t.push_str(&out.transcript);
                t.chars()
                    .rev()
                    .take(8000)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect()
            },
            duration_ms: started.elapsed().as_millis() as u64,
            turns: 1,
            crashed: false,
            timed_out: false,
            raw_artifact_path: None,
            cost_usd_micros: out.cost_usd_micros,
            usage_in: out.usage_in,
            usage_out: out.usage_out,
        })
    }

    async fn status(&self, _handle: &WorkerHandle) -> Result<crate::WorkerStatus, WanaxError> {
        Ok(crate::WorkerStatus::Exited)
    }

    async fn cancel(&self, _handle: &WorkerHandle) -> Result<(), WanaxError> {
        Ok(()) // process is already reaped inside start
    }

    async fn collect_receipt(
        &self,
        ctx: &WorkerContext,
        handle: &WorkerHandle,
    ) -> Result<wanax_core::types::Receipt, WanaxError> {
        crate::receipt_from_handle(ctx, handle, "acp")
    }
}
