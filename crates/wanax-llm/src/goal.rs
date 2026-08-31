use crate::provider::CompletionClient;
use crate::LlmUsage;
use wanax_core::config::ResolvedConfig;

#[derive(Debug, Clone)]
pub struct GoalSelfReview {
    pub degraded: bool,
    pub mode: &'static str,
    pub notes: String,
    pub usage: Option<LlmUsage>,
}

pub fn self_review_degraded(reviewer_model: Option<&str>, inner_model: &str) -> bool {
    match reviewer_model.map(str::trim).filter(|s| !s.is_empty()) {
        None => true,
        Some(model) => model == inner_model,
    }
}

pub fn mechanical_self_review(changed: &[String], test_exit_code: i32) -> GoalSelfReview {
    GoalSelfReview {
        degraded: true,
        mode: "mechanical",
        notes: format!(
            "re-ran tests exit={test_exit_code}; diff: {}",
            changed.join(", ")
        ),
        usage: None,
    }
}

pub fn pick_review_client(cfg: &ResolvedConfig) -> Option<Box<dyn CompletionClient>> {
    if self_review_degraded(cfg.file.reviewer.model.as_deref(), &cfg.file.inner.model) {
        return None;
    }
    let key = std::env::var("WANAX_INNER_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())?;
    let kind = crate::provider::ProviderKind::parse(&cfg.file.inner.provider).ok()?;
    crate::provider::LiveClient::new(kind, key, cfg.file.inner.base_url.clone())
        .ok()
        .map(|c| Box::new(c) as _)
}

pub async fn run_self_review(
    reviewer_model: Option<&str>,
    inner_model: &str,
    client: Option<&dyn CompletionClient>,
    changed: &[String],
    test_exit_code: i32,
    excerpt: &str,
) -> GoalSelfReview {
    let Some(model) = reviewer_model.map(str::trim).filter(|s| !s.is_empty()) else {
        return mechanical_self_review(changed, test_exit_code);
    };
    if self_review_degraded(Some(model), inner_model) || client.is_none() {
        return mechanical_self_review(changed, test_exit_code);
    }
    let Some(client) = client else {
        return mechanical_self_review(changed, test_exit_code);
    };
    let system = "You are a Wanax inner reviewer. Reply with JSON only: {\"notes\":\"...\"}. \
Do not accept, merge, or claim outer verdict. Do not write files.";
    let user = format!(
        "test_exit_code={test_exit_code}\nfiles:\n{}\nexcerpt:\n{excerpt}\n",
        changed.join("\n")
    );
    match client.complete(system, &user, model).await {
        Ok(completion) => GoalSelfReview {
            degraded: false,
            mode: "semantic",
            notes: completion.text.clone(),
            usage: Some(LlmUsage {
                chars_in: (system.len() + user.len()) as u64,
                chars_out: completion.text.len() as u64,
                prompt_tokens: completion.prompt_tokens,
                completion_tokens: completion.completion_tokens,
                raw_json: completion.text,
            }),
        },
        Err(_) => mechanical_self_review(changed, test_exit_code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_same_model_is_degraded() {
        assert!(self_review_degraded(None, "inner"));
        assert!(self_review_degraded(Some(""), "inner"));
        assert!(self_review_degraded(Some("inner"), "inner"));
        assert!(!self_review_degraded(Some("reviewer"), "inner"));
    }
}
