use async_trait::async_trait;
use serde_json::Value;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;
use wanax_core::error::{ErrorCode, WanaxError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Anthropic,
    OpenAi,
    OpenAiCompat,
}

impl ProviderKind {
    pub fn parse(s: &str) -> Result<Self, WanaxError> {
        match s {
            "anthropic" => Ok(Self::Anthropic),
            "openai" => Ok(Self::OpenAi),
            "openai_compat" => Ok(Self::OpenAiCompat),
            other => Err(WanaxError::new(
                ErrorCode::CommanderSchema,
                format!("unknown LLM provider: {other}"),
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Completion {
    pub text: String,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}

#[async_trait]
pub trait CompletionClient: Send + Sync {
    async fn complete(
        &self,
        system: &str,
        user: &str,
        model: &str,
    ) -> Result<Completion, WanaxError>;
}

pub struct LiveClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    kind: ProviderKind,
}

impl LiveClient {
    pub fn new(
        kind: ProviderKind,
        api_key: String,
        base_url: Option<String>,
    ) -> Result<Self, WanaxError> {
        let base_url = base_url.unwrap_or_else(|| default_base(kind).to_string());
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(map_llm_err)?;
        Ok(Self {
            http,
            api_key,
            base_url,
            kind,
        })
    }
}

fn default_base(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Anthropic => "https://api.anthropic.com",
        ProviderKind::OpenAi | ProviderKind::OpenAiCompat => "https://api.openai.com/v1",
    }
}

#[async_trait]
impl CompletionClient for LiveClient {
    async fn complete(
        &self,
        system: &str,
        user: &str,
        model: &str,
    ) -> Result<Completion, WanaxError> {
        match self.kind {
            ProviderKind::Anthropic => anthropic_complete(self, system, user, model).await,
            ProviderKind::OpenAi | ProviderKind::OpenAiCompat => {
                openai_complete(self, system, user, model).await
            }
        }
    }
}

async fn openai_complete(
    client: &LiveClient,
    system: &str,
    user: &str,
    model: &str,
) -> Result<Completion, WanaxError> {
    let url = format!("{}/chat/completions", client.base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ]
    });
    let res = client
        .http
        .post(url)
        .bearer_auth(&client.api_key)
        .json(&body)
        .send()
        .await
        .map_err(map_llm_err)?;
    let status = res.status();
    let value: Value = res.json().await.map_err(map_llm_err)?;
    if !status.is_success() {
        return Err(WanaxError::new(
            ErrorCode::CommanderSchema,
            format!("commander schema invalid: HTTP {status}"),
        ));
    }
    parse_openai_body(&value)
}

async fn anthropic_complete(
    client: &LiveClient,
    system: &str,
    user: &str,
    model: &str,
) -> Result<Completion, WanaxError> {
    let url = format!("{}/v1/messages", client.base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "max_tokens": 4096,
        "system": system,
        "messages": [{"role": "user", "content": user}]
    });
    let res = client
        .http
        .post(url)
        .header("x-api-key", &client.api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .await
        .map_err(map_llm_err)?;
    let status = res.status();
    let value: Value = res.json().await.map_err(map_llm_err)?;
    if !status.is_success() {
        return Err(WanaxError::new(
            ErrorCode::CommanderSchema,
            format!("commander schema invalid: HTTP {status}"),
        ));
    }
    parse_anthropic_body(&value)
}

fn map_llm_err(e: impl std::fmt::Display) -> WanaxError {
    WanaxError::new(
        ErrorCode::CommanderSchema,
        format!("commander schema invalid: {e}"),
    )
}

pub fn parse_openai_body(value: &Value) -> Result<Completion, WanaxError> {
    let text = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| WanaxError::from_code(ErrorCode::CommanderSchema))?
        .to_string();
    let prompt_tokens = value
        .pointer("/usage/prompt_tokens")
        .and_then(Value::as_u64);
    let completion_tokens = value
        .pointer("/usage/completion_tokens")
        .and_then(Value::as_u64);
    Ok(Completion {
        text,
        prompt_tokens,
        completion_tokens,
    })
}

pub fn parse_anthropic_body(value: &Value) -> Result<Completion, WanaxError> {
    let text = value
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .ok_or_else(|| WanaxError::from_code(ErrorCode::CommanderSchema))?
        .to_string();
    let prompt_tokens = value.pointer("/usage/input_tokens").and_then(Value::as_u64);
    let completion_tokens = value
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64);
    Ok(Completion {
        text,
        prompt_tokens,
        completion_tokens,
    })
}

pub struct FixtureClient {
    remaining: Mutex<VecDeque<Value>>,
    kind: ProviderKind,
}

impl FixtureClient {
    pub fn load_dir(dir: &Path) -> Result<Self, WanaxError> {
        let path = dir.join("cassette.json");
        let text = std::fs::read_to_string(&path).map_err(|e| {
            WanaxError::new(
                ErrorCode::CommanderSchema,
                format!("commander schema invalid: missing cassette: {e}"),
            )
        })?;
        let root: Value = serde_json::from_str(&text)
            .map_err(|_| WanaxError::from_code(ErrorCode::CommanderSchema))?;
        let kind = root
            .get("provider")
            .and_then(Value::as_str)
            .map(ProviderKind::parse)
            .transpose()?
            .unwrap_or(ProviderKind::OpenAiCompat);
        let mut q = VecDeque::new();
        if let Some(calls) = root.get("calls").and_then(Value::as_array) {
            for call in calls {
                let body = call.get("body").cloned().unwrap_or_else(|| call.clone());
                q.push_back(body);
            }
        }
        Ok(Self {
            remaining: Mutex::new(q),
            kind,
        })
    }
}

#[async_trait]
impl CompletionClient for FixtureClient {
    async fn complete(
        &self,
        _system: &str,
        _user: &str,
        _model: &str,
    ) -> Result<Completion, WanaxError> {
        let body = self
            .remaining
            .lock()
            .map_err(|_| WanaxError::from_code(ErrorCode::CommanderSchema))?
            .pop_front()
            .ok_or_else(|| {
                WanaxError::new(
                    ErrorCode::CommanderSchema,
                    "commander schema invalid: cassette exhausted",
                )
            })?;
        match self.kind {
            ProviderKind::Anthropic => parse_anthropic_body(&body),
            ProviderKind::OpenAi | ProviderKind::OpenAiCompat => parse_openai_body(&body),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_usage_and_content() {
        let v = json!({
            "choices": [{"message": {"content": "{\"title\":\"t\"}"}}],
            "usage": {"prompt_tokens": 11, "completion_tokens": 7}
        });
        let c = parse_openai_body(&v).unwrap();
        assert_eq!(c.text, "{\"title\":\"t\"}");
        assert_eq!(c.prompt_tokens, Some(11));
        assert_eq!(c.completion_tokens, Some(7));
    }

    #[test]
    fn anthropic_usage_and_content() {
        let v = json!({
            "content": [{"type": "text", "text": "ok"}],
            "usage": {"input_tokens": 3, "output_tokens": 4}
        });
        let c = parse_anthropic_body(&v).unwrap();
        assert_eq!(c.text, "ok");
        assert_eq!(c.prompt_tokens, Some(3));
        assert_eq!(c.completion_tokens, Some(4));
    }
}
