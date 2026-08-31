use regex::Regex;
use std::sync::OnceLock;

fn re_auth() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(authorization:\s*)(bearer\s+)?\S+").expect("auth re"))
}

fn re_sk() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"sk-[A-Za-z0-9_-]+").expect("sk re"))
}

fn re_ghp() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"ghp_[A-Za-z0-9_]+").expect("ghp re"))
}

pub fn redact(input: &str) -> String {
    let mut s = re_auth().replace_all(input, "${1}[REDACTED]").into_owned();
    s = re_sk().replace_all(&s, "[REDACTED]").into_owned();
    s = re_ghp().replace_all(&s, "[REDACTED]").into_owned();
    for key in ["WANAX_COMMANDER_API_KEY", "WANAX_INNER_API_KEY"] {
        if let Ok(val) = std::env::var(key) {
            if !val.is_empty() {
                s = s.replace(&val, "[REDACTED]");
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_key_prefixes_and_auth_headers() {
        let raw = "Authorization: Bearer sk-abc123XYZ token ghp_deadbeef99";
        let out = redact(raw);
        assert!(!out.contains("sk-"), "{out}");
        assert!(!out.contains("ghp_"), "{out}");
        assert!(!out.contains("Bearer sk-"), "{out}");
        assert!(out.contains("[REDACTED]"));
    }
}
