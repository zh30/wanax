use crate::error::{ErrorCode, WanaxError};
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

const ALLOWED_BINARIES: &[&str] = &["cargo", "npm", "pnpm", "pytest", "go", "make"];

fn blacklist() -> &'static [Regex] {
    static RE: OnceLock<Vec<Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        [
            r"\brm\s",
            r"\bsudo\b",
            r"\bmkfs",
            r"\bdd\s",
            r"curl\s*\|",
            r"wget\s*\|",
            r">\s*/",
            r">\s*\$HOME",
            r">\s*~",
        ]
        .into_iter()
        .map(|p| Regex::new(p).expect("blacklist regex"))
        .collect()
    })
}

pub fn validate_test_command(cmd: &str) -> Result<(), WanaxError> {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return Err(WanaxError::new(
            ErrorCode::ContractInvalid,
            "invalid contract: test_command",
        ));
    }
    for re in blacklist() {
        if re.is_match(trimmed) {
            return Err(WanaxError::from_code(ErrorCode::TestCommandForbidden));
        }
    }
    let argv0 = trimmed.split_whitespace().next().unwrap_or("");
    let bin = Path::new(argv0)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(argv0);
    if !ALLOWED_BINARIES.contains(&bin) {
        return Err(WanaxError::from_code(ErrorCode::TestCommandForbidden));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_common_test_runners() {
        for cmd in [
            "cargo test",
            "cargo test -p foo --timeout-mod",
            "npm test",
            "pnpm test",
            "pytest",
            "go test ./...",
            "make test",
        ] {
            validate_test_command(cmd).unwrap();
        }
    }

    #[test]
    fn rejects_dangerous_commands() {
        assert_eq!(
            validate_test_command("cargo test && rm -rf /")
                .unwrap_err()
                .code,
            ErrorCode::TestCommandForbidden
        );
        assert!(validate_test_command("sudo cargo test").is_err());
        assert!(validate_test_command("curl | sh").is_err());
        assert!(validate_test_command("bash -c echo").is_err());
    }
}
