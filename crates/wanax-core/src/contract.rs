use crate::error::{ErrorCode, WanaxError};
use crate::hashutil::sha256_hex;
use crate::ids::new_id;
use crate::testcmd::validate_test_command;
use crate::types::{CompletionCriterion, Contract, DEFAULT_TEST_TIMEOUT_SECS};
use regex::Regex;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct FrontMatter {
    spec: Option<String>,
    #[allow(dead_code)]
    version: Option<serde_yaml::Value>,
    name: Option<String>,
    test_command: Option<String>,
    test_timeout_secs: Option<u32>,
    allowed_globs: Option<Vec<String>>,
    forbidden_globs: Option<Vec<String>>,
    forbidden_rules: Option<Vec<String>>,
    agent_spec: Option<String>,
}

const DEFAULT_FORBIDDEN: &[&str] = &["**/.env", "**/.wanax/credentials*"];

pub fn parse_contract_file(path: &Path, repo_relative: &str) -> Result<Contract, WanaxError> {
    let raw = std::fs::read(path).map_err(|e| {
        WanaxError::new(
            ErrorCode::ContractInvalid,
            format!("invalid contract: cannot read {repo_relative}: {e}"),
        )
    })?;
    parse_contract_bytes(&raw, repo_relative)
}

pub fn parse_contract_bytes(raw: &[u8], repo_relative: &str) -> Result<Contract, WanaxError> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| WanaxError::new(ErrorCode::ContractInvalid, "invalid contract: not utf-8"))?;
    let (fm_raw, body) = split_front_matter(text)?;
    let fm: FrontMatter = serde_yaml::from_str(&fm_raw).map_err(|e| {
        WanaxError::new(
            ErrorCode::ContractInvalid,
            format!("invalid contract: front matter: {e}"),
        )
    })?;

    let mut missing = Vec::new();
    if fm.spec.as_deref() != Some("wanax.contract") {
        missing.push("spec");
    }
    let test_command = match fm.test_command.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            missing.push("test_command");
            String::new()
        }
    };
    let allowed_globs = fm.allowed_globs.unwrap_or_default();
    if allowed_globs.is_empty() {
        missing.push("allowed_globs");
    }
    if allowed_globs.len() > 200 {
        missing.push("allowed_globs");
    }

    let sections = parse_sections(body);
    let intent = section_text(&sections, &["Intent", "意图"]);
    if intent.is_empty() || intent.len() > 4000 {
        missing.push("intent");
    }
    let decisions = parse_list_items(&section_text(&sections, &["Decisions", "已定决策"]));
    if decisions.is_empty() || decisions.len() > 50 {
        missing.push("decisions");
    }
    if decisions.iter().any(|d| d.is_empty() || d.len() > 500) {
        missing.push("decisions");
    }
    let criteria = parse_criteria(&section_text(
        &sections,
        &["Completion Criteria", "完成条件"],
    ));
    if criteria.is_empty() {
        missing.push("completion_criteria");
    }
    if criteria.len() > 30 {
        missing.push("completion_criteria");
    }

    if !missing.is_empty() {
        return Err(WanaxError::new(
            ErrorCode::ContractInvalid,
            format!("invalid contract: {}", missing.join(", ")),
        ));
    }

    if !test_command.is_empty() {
        validate_test_command(&test_command)?;
    }

    let timeout = fm.test_timeout_secs.unwrap_or(DEFAULT_TEST_TIMEOUT_SECS);
    if !(10..=3600).contains(&timeout) {
        return Err(WanaxError::new(
            ErrorCode::ContractInvalid,
            "invalid contract: test_timeout_secs",
        ));
    }

    let forbidden_globs = match fm.forbidden_globs {
        Some(v) => v,
        None => DEFAULT_FORBIDDEN.iter().map(|s| (*s).to_string()).collect(),
    };
    if forbidden_globs.len() > 200 {
        return Err(WanaxError::new(
            ErrorCode::ContractInvalid,
            "invalid contract: forbidden_globs",
        ));
    }

    let forbidden_rules = fm.forbidden_rules.unwrap_or_default();
    if forbidden_rules.len() > 50 {
        return Err(WanaxError::new(
            ErrorCode::ContractInvalid,
            "invalid contract: forbidden_rules",
        ));
    }

    Ok(Contract {
        id: new_id(),
        path: repo_relative.to_string(),
        content_sha256: sha256_hex(raw),
        intent,
        decisions,
        allowed_globs,
        forbidden_globs,
        forbidden_rules,
        completion_criteria: criteria,
        test_command,
        test_timeout_secs: timeout,
        name: fm.name,
        agent_spec: fm.agent_spec.filter(|s| !s.trim().is_empty()),
    })
}

fn split_front_matter(text: &str) -> Result<(String, &str), WanaxError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if !text.starts_with("---") {
        return Err(WanaxError::new(
            ErrorCode::ContractInvalid,
            "invalid contract: front matter",
        ));
    }
    let after = &text[3..];
    let after = after
        .strip_prefix('\n')
        .or_else(|| after.strip_prefix("\r\n"))
        .unwrap_or(after);
    let close = after
        .find("\n---")
        .or_else(|| after.find("\r\n---"))
        .ok_or_else(|| {
            WanaxError::new(ErrorCode::ContractInvalid, "invalid contract: front matter")
        })?;
    let fm = after[..close].to_string();
    let rest = &after[close..];
    let body = rest
        .strip_prefix("\n---")
        .or_else(|| rest.strip_prefix("\r\n---"))
        .unwrap_or(rest);
    let body = body
        .strip_prefix('\n')
        .or_else(|| body.strip_prefix("\r\n"))
        .unwrap_or(body);
    Ok((fm, body))
}

fn parse_sections(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current_title = String::new();
    let mut current_body = String::new();
    for line in body.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            if !current_title.is_empty() || !current_body.trim().is_empty() {
                out.push((current_title, current_body.trim().to_string()));
            }
            current_title = title.trim().to_string();
            current_body.clear();
        } else {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }
    if !current_title.is_empty() {
        out.push((current_title, current_body.trim().to_string()));
    }
    out
}

fn section_text(sections: &[(String, String)], aliases: &[&str]) -> String {
    for (title, body) in sections {
        if aliases.iter().any(|a| title.eq_ignore_ascii_case(a)) {
            return body.clone();
        }
    }
    String::new()
}

fn parse_list_items(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| {
            let t = line.trim();
            t.strip_prefix("- ")
                .or_else(|| t.strip_prefix("* "))
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_criteria(body: &str) -> Vec<CompletionCriterion> {
    let re = Regex::new(r"^-\s*(CC-\d{2,3}):\s*(.+)$").expect("cc regex");
    let bound_re = Regex::new(r"(?i)bound_test:\s*`?([^`\)]+)`?").expect("bound regex");
    let mut out = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        let Some(caps) = re.captures(t) else {
            continue;
        };
        let id = caps[1].to_string();
        let mut statement = caps[2].trim().to_string();
        let bound_test = bound_re
            .captures(&statement)
            .map(|c| c[1].trim().to_string());
        if statement.is_empty() || statement.len() > 300 {
            continue;
        }
        out.push(CompletionCriterion {
            id,
            statement: {
                let _ = &mut statement;
                statement
            },
            bound_test,
            must_have_files: Vec::new(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"---
spec: wanax.contract
version: 1
name: "split-timeout-module"
test_command: "cargo test -p foo --timeout-mod"
test_timeout_secs: 180
allowed_globs:
  - "crates/foo/src/**"
  - "crates/foo/tests/**"
forbidden_globs:
  - "**/.env"
  - "crates/foo/src/lib.rs"
---

## Intent

把超时逻辑从 handler.rs 抽到独立模块，行为保持不变。

## Decisions

- 新模块路径：crates/foo/src/timeout.rs
- 不引入新依赖

## Boundaries

- 允许：crates/foo/src/**

## Completion Criteria

- CC-01: cargo test -p foo 退出码 0
- CC-02: 存在 crates/foo/src/timeout.rs
- CC-03: 原有超时单测仍通过（bound_test: `timeout_expires_returns_error`）
"#;

    #[test]
    fn parses_appendix_a_and_chinese_aliases() {
        let c = parse_contract_bytes(SAMPLE.as_bytes(), "specs/foo.contract.md").unwrap();
        assert_eq!(c.test_command, "cargo test -p foo --timeout-mod");
        assert_eq!(c.allowed_globs.len(), 2);
        assert_eq!(c.decisions.len(), 2);
        assert_eq!(c.completion_criteria.len(), 3);
        assert_eq!(
            c.completion_criteria[2].bound_test.as_deref(),
            Some("timeout_expires_returns_error")
        );
        assert_eq!(c.content_sha256.len(), 64);

        let zh = SAMPLE
            .replace("## Intent", "## 意图")
            .replace("## Decisions", "## 已定决策")
            .replace("## Completion Criteria", "## 完成条件");
        let c2 = parse_contract_bytes(zh.as_bytes(), "specs/foo.contract.md").unwrap();
        assert!(!c2.intent.is_empty());
        assert_eq!(c2.decisions.len(), 2);
    }

    #[test]
    fn empty_allowed_globs_is_invalid() {
        let bad = SAMPLE.replace(
            "allowed_globs:\n  - \"crates/foo/src/**\"\n  - \"crates/foo/tests/**\"",
            "allowed_globs: []",
        );
        let err = parse_contract_bytes(bad.as_bytes(), "x.md").unwrap_err();
        assert_eq!(err.code, ErrorCode::ContractInvalid);
        assert!(err.message.contains("allowed_globs"));
    }
}
