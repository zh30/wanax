use std::collections::HashMap;
use std::path::Path;

/// Environment keys that must never reach a spawned child (worker CLI, test
/// harness, commander CLI, verifier plugin). Keep in sync with the security
/// tests in wanax-worker and the e2e suite.
pub const FORBIDDEN_ENV: &[&str] = &[
    "GIT_ASKPASS",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "WANAX_COMMANDER_API_KEY",
    "WANAX_INNER_API_KEY",
];

/// Innocuous keys every child gets when present in the parent environment.
pub const BASE_ENV_KEYS: &[&str] = &[
    "PATH", "HOME", "USER", "TMPDIR", "LANG", "LC_ALL", "TERM", "SHELL",
];

/// Whitelist base environment for any child process wanax spawns.
/// Everything not listed here is dropped.
pub fn base_child_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    for key in BASE_ENV_KEYS {
        if let Ok(v) = std::env::var(key) {
            env.insert((*key).to_string(), v);
        }
    }
    env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
    env
}

/// Copy explicitly listed keys from the parent environment, unless the key is
/// on the forbidden list. Use for contract-declared `test.pass_env`.
pub fn with_passthrough(env: &mut HashMap<String, String>, keys: &[String]) {
    for key in keys {
        let trimmed = key.trim();
        if trimmed.is_empty() || FORBIDDEN_ENV.contains(&trimmed) {
            continue;
        }
        if let Ok(v) = std::env::var(trimmed) {
            env.insert(trimmed.to_string(), v);
        }
    }
}

/// Point git at the wanax-generated global config (credential helper emptied,
/// gpgsign off) instead of the user's real ~/.gitconfig, and ignore the
/// system config. No repository config file is modified.
pub fn with_git_config(env: &mut HashMap<String, String>, gitconfig: &Path) {
    env.insert("GIT_CONFIG_GLOBAL".into(), gitconfig.display().to_string());
    env.insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
}

/// Prepend a directory to PATH (used for the git wrapper shim).
pub fn with_prepend_path(env: &mut HashMap<String, String>, dir: &Path) {
    let path = match env.get("PATH") {
        Some(p) => format!("{}:{p}", dir.display()),
        None => dir.display().to_string(),
    };
    env.insert("PATH".into(), path);
}

pub fn env_has_forbidden(env: &HashMap<String, String>) -> bool {
    FORBIDDEN_ENV.iter().any(|k| env.contains_key(*k))
}

/// Is this name on the never-pass list (wanax's own secrets, git creds, ssh)?
pub fn is_forbidden(name: &str) -> bool {
    FORBIDDEN_ENV.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_env_has_no_forbidden_keys() {
        for k in FORBIDDEN_ENV {
            std::env::set_var(k, "leak-check");
        }
        let env = base_child_env();
        assert!(!env_has_forbidden(&env));
        assert_eq!(
            env.get("GIT_TERMINAL_PROMPT").map(String::as_str),
            Some("0")
        );
        for k in FORBIDDEN_ENV {
            std::env::remove_var(k);
        }
    }

    #[test]
    fn passthrough_never_includes_forbidden() {
        std::env::set_var("MY_TEST_FLAG", "1");
        std::env::set_var("GH_TOKEN", "x");
        let mut env = base_child_env();
        with_passthrough(&mut env, &["MY_TEST_FLAG".into(), "GH_TOKEN".into()]);
        assert_eq!(env.get("MY_TEST_FLAG").map(String::as_str), Some("1"));
        assert!(!env.contains_key("GH_TOKEN"));
        std::env::remove_var("MY_TEST_FLAG");
        std::env::remove_var("GH_TOKEN");
    }

    #[test]
    fn git_config_env_points_at_wanax_file() {
        let mut env = base_child_env();
        with_git_config(&mut env, Path::new("/repo/.wanax/gitconfig"));
        assert_eq!(
            env.get("GIT_CONFIG_GLOBAL").map(String::as_str),
            Some("/repo/.wanax/gitconfig")
        );
        assert_eq!(
            env.get("GIT_CONFIG_NOSYSTEM").map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn is_forbidden_blocks_known_names() {
        for name in FORBIDDEN_ENV {
            assert!(is_forbidden(name), "{name}");
        }
        assert!(is_forbidden("GH_TOKEN"));
        assert!(is_forbidden("SSH_AUTH_SOCK"));
        assert!(is_forbidden("WANAX_COMMANDER_API_KEY"));
        assert!(is_forbidden("WANAX_INNER_API_KEY"));
    }

    #[test]
    fn is_forbidden_allows_benign_names() {
        assert!(!is_forbidden("PATH"));
        assert!(!is_forbidden("HOME"));
        assert!(!is_forbidden("ANTHROPIC_API_KEY"));
    }
}
