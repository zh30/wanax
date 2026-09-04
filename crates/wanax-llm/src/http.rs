use crate::jsonutil::extract_json_object;
use crate::provider::{Completion, CompletionClient};
use crate::{
    parse_dispatch_plan, parse_verdict, Commander, DispatchContext, DispatchPlan, LlmUsage,
    VerdictContext, VerdictDraft,
};
use async_trait::async_trait;
use std::sync::Arc;
use wanax_core::error::{ErrorCode, WanaxError};

const DISPATCH_SYSTEM: &str = "You are the Wanax commander. Reply with JSON only: \
{\"title\":\"...\",\"instruction\":\"...\"}. title is 1-120 characters. \
instruction is 1-8000 characters and MUST include allowed/forbidden boundaries, \
test_command, and completion criteria. Do not write repository files.";

const VERDICT_SYSTEM: &str = "You are the Wanax commander. Reply with JSON only: \
{\"decision\":\"accept|reject|rework|escalate\",\"reason\":\"...\",\"files_reviewed\":[\"...\"]}. \
Do not accept if outer tests failed or boundaries were violated. Do not write repository files.";

pub struct HttpCommander {
    client: Arc<dyn CompletionClient>,
    model: String,
}

impl HttpCommander {
    pub fn new(client: Arc<dyn CompletionClient>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
        }
    }

    fn usage_from(completion: &Completion, system: &str, user: &str, raw_json: String) -> LlmUsage {
        LlmUsage {
            chars_in: (system.len() + user.len()) as u64,
            chars_out: completion.text.len() as u64,
            prompt_tokens: completion.prompt_tokens,
            completion_tokens: completion.completion_tokens,
            raw_json,
        }
    }
}

#[async_trait]
impl Commander for HttpCommander {
    async fn dispatch_plan(
        &self,
        ctx: &DispatchContext,
    ) -> Result<(DispatchPlan, LlmUsage), WanaxError> {
        let user = crate::format_dispatch_instruction(ctx);
        let completion = self
            .client
            .complete(DISPATCH_SYSTEM, &user, &self.model)
            .await?;
        let json = extract_json_object(&completion.text)
            .ok_or_else(|| WanaxError::from_code(ErrorCode::CommanderSchema))?;
        let (plan, _) = parse_dispatch_plan(json)?;
        Ok((
            plan,
            Self::usage_from(&completion, DISPATCH_SYSTEM, &user, json.to_string()),
        ))
    }

    async fn verdict(&self, ctx: &VerdictContext) -> Result<(VerdictDraft, LlmUsage), WanaxError> {
        let user = format!(
            "outer_test_exit_code={}\nboundary_ok={}\nclaimed_pass={}\nrework_count={}\n\
changed_files:\n{}\n\ndiffstat:\n{}\n\nexcerpt:\n{}\n",
            ctx.outer_test_exit_code,
            ctx.boundary_ok,
            ctx.receipt.claimed_pass,
            ctx.rework_count,
            ctx.changed_files.join("\n"),
            ctx.diffstat,
            ctx.outer_test_excerpt
        );
        let completion = self
            .client
            .complete(VERDICT_SYSTEM, &user, &self.model)
            .await?;
        let json = extract_json_object(&completion.text)
            .ok_or_else(|| WanaxError::from_code(ErrorCode::CommanderSchema))?;
        let (draft, _) = parse_verdict(json)?;
        Ok((
            draft,
            Self::usage_from(&completion, VERDICT_SYSTEM, &user, json.to_string()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::FixtureClient;
    use crate::DispatchContext;
    use serde_json::json;
    use wanax_core::types::{CompletionCriterion, Contract};

    #[tokio::test]
    async fn fixture_client_serves_dispatch_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let cassette = json!({
            "provider": "openai",
            "calls": [{
                "body": {
                    "choices": [{"message": {"content": "{\"title\":\"add-fn\",\"instruction\":\"do the work. allowed: src/**. test_command: cargo test. CC-01: pass\"}"}}],
                    "usage": {"prompt_tokens": 9, "completion_tokens": 4}
                }
            }]
        });
        std::fs::write(dir.path().join("cassette.json"), cassette.to_string()).unwrap();
        let client = FixtureClient::load_dir(dir.path()).unwrap();
        let cmd = HttpCommander::new(Arc::new(client), "commander");
        let ctx = DispatchContext {
            contract: Contract {
                id: "wx_01AAAAAAAAAAAAAAAAAAAAAAAA".into(),
                path: "specs/a.md".into(),
                content_sha256: "ab".repeat(32),
                intent: "add".into(),
                decisions: vec!["d".into()],
                allowed_globs: vec!["src/**".into()],
                forbidden_globs: vec!["**/.env".into()],
                forbidden_rules: vec![],
                completion_criteria: vec![CompletionCriterion {
                    id: "CC-01".into(),
                    statement: "pass".into(),
                    bound_test: None,
                    must_have_files: vec![],
                }],
                test_command: "cargo test".into(),
                test_timeout_secs: 30,
                name: Some("add-fn".into()),
                agent_spec: None,
            },
            rework_notes: None,
        };
        let (plan, usage) = cmd.dispatch_plan(&ctx).await.unwrap();
        let DispatchPlan::Single(draft) = plan else {
            panic!("expected single dispatch");
        };
        assert_eq!(draft.title, "add-fn");
        assert!(draft.instruction.contains("cargo test"));
        assert_eq!(usage.prompt_tokens, Some(9));
        assert_eq!(usage.completion_tokens, Some(4));
        assert!(!usage.charge_units().2);
    }
}
