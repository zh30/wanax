use chrono::{SecondsFormat, Utc};

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn parse_rfc3339(s: &str) -> Result<chrono::DateTime<Utc>, String> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_rfc3339_utc() {
        let s = now_rfc3339();
        assert!(s.ends_with('Z'), "{s}");
        parse_rfc3339(&s).unwrap();
    }
}
