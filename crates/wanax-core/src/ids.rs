use crate::error::{ErrorCode, WanaxError};
use regex::Regex;
use std::sync::OnceLock;
use ulid::Ulid;

pub const ID_PREFIX: &str = "wx_";
pub const ID_PATTERN: &str = r"^wx_[0-9A-HJKMNP-TV-Z]{26}$";

fn id_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(ID_PATTERN).expect("id regex"))
}

pub fn new_id() -> String {
    format!("{ID_PREFIX}{}", Ulid::new())
}

pub fn validate_id(id: &str) -> Result<(), WanaxError> {
    if id_re().is_match(id) {
        Ok(())
    } else {
        Err(WanaxError::with_detail(
            ErrorCode::ContractInvalid,
            format!("invalid id {id}"),
        ))
    }
}

pub fn is_valid_id(id: &str) -> bool {
    id_re().is_match(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_match_spec() {
        let id = new_id();
        assert!(id.starts_with("wx_"));
        assert_eq!(id.len(), 3 + 26);
        validate_id(&id).unwrap();
    }

    #[test]
    fn rejects_legacy_prefixes() {
        assert!(!is_valid_id("nf_01K3QABCDEFGHJKLMNPQRSTVW"));
        assert!(!is_valid_id("aq_01K3QABCDEFGHJKLMNPQRSTVW"));
        assert!(!is_valid_id("wx_not-a-ulid"));
    }
}
