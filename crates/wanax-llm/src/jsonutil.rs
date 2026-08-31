/// Pull the first JSON object from a model reply (raw or fenced).
pub fn extract_json_object(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    let body = if let Some(rest) = trimmed.strip_prefix("```json") {
        rest.split_once("```")
            .map(|(a, _)| a.trim())
            .unwrap_or(rest.trim())
    } else if let Some(rest) = trimmed.strip_prefix("```") {
        rest.split_once("```")
            .map(|(a, _)| a.trim())
            .unwrap_or(rest.trim())
    } else {
        trimmed
    };
    let start = body.find('{')?;
    let bytes = body.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&body[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fenced_and_raw_objects() {
        let fenced = "```json\n{\"title\":\"t\",\"instruction\":\"do it\"}\n```";
        assert_eq!(
            extract_json_object(fenced),
            Some("{\"title\":\"t\",\"instruction\":\"do it\"}")
        );
        assert_eq!(
            extract_json_object("prefix {\"a\":1} suffix"),
            Some("{\"a\":1}")
        );
    }
}
