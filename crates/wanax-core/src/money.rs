/// 1 USD = 1_000_000 micros. Never persist IEEE floats for money.
pub const USD_MICROS_PER_UNIT: i64 = 1_000_000;

pub fn parse_usd_decimal(s: &str) -> Result<i64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty usd amount".into());
    }
    let neg = s.starts_with('-');
    let s = s.strip_prefix(['-', '+']).unwrap_or(s);
    let (whole, frac) = match s.split_once('.') {
        Some((w, f)) => (w, f),
        None => (s, ""),
    };
    if whole.is_empty() || !whole.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("invalid usd amount {s}"));
    }
    if frac.chars().any(|c| !c.is_ascii_digit()) {
        return Err(format!("invalid usd fraction {s}"));
    }
    if frac.len() > 6 {
        return Err("usd precision exceeds micros".into());
    }
    let whole_i: i64 = whole.parse().map_err(|_| format!("usd overflow {s}"))?;
    let mut frac_padded = frac.to_string();
    while frac_padded.len() < 6 {
        frac_padded.push('0');
    }
    let frac_i: i64 = frac_padded
        .parse()
        .map_err(|_| format!("usd overflow {s}"))?;
    let mut micros = whole_i
        .checked_mul(USD_MICROS_PER_UNIT)
        .and_then(|v| v.checked_add(frac_i))
        .ok_or_else(|| format!("usd overflow {s}"))?;
    if neg {
        micros = -micros;
    }
    Ok(micros)
}

pub fn format_usd(micros: i64) -> String {
    let neg = micros < 0;
    let micros = micros.unsigned_abs();
    let whole = micros / USD_MICROS_PER_UNIT as u64;
    let frac = micros % USD_MICROS_PER_UNIT as u64;
    if neg {
        format!("-{whole}.{frac:06}")
    } else {
        format!("{whole}.{frac:06}")
    }
}

pub fn format_usd_4(micros: i64) -> String {
    let s = format_usd(micros);
    if let Some((w, f)) = s.split_once('.') {
        format!("{w}.{}", &f[..4.min(f.len())])
    } else {
        s
    }
}

pub fn estimate_cost_micros(
    chars_in: u64,
    chars_out: u64,
    usd_per_million_in: i64,
    usd_per_million_out: i64,
) -> i64 {
    // cost = chars / 1e6 * rate, in micros (rate already micros per 1M chars)
    let inn = (chars_in as i128 * usd_per_million_in as i128) / 1_000_000;
    let out = (chars_out as i128 * usd_per_million_out as i128) / 1_000_000;
    (inn + out) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_budget_is_five_dollars() {
        assert_eq!(parse_usd_decimal("5.00").unwrap(), 5_000_000);
        assert_eq!(format_usd_4(5_000_000), "5.0000");
    }

    #[test]
    fn estimate_uses_integer_math() {
        // 1M chars in at $10/1M = $10 = 10_000_000 micros
        assert_eq!(
            estimate_cost_micros(1_000_000, 0, 10_000_000, 50_000_000),
            10_000_000
        );
    }

    #[test]
    fn parse_rejects_negative_and_malformed() {
        assert_eq!(parse_usd_decimal("").unwrap_err(), "empty usd amount");
        assert_eq!(
            parse_usd_decimal("a.5").unwrap_err(),
            "invalid usd amount a.5"
        );
        assert_eq!(
            parse_usd_decimal("1.x").unwrap_err(),
            "invalid usd fraction 1.x"
        );
        assert_eq!(
            parse_usd_decimal("1.1234567").unwrap_err(),
            "usd precision exceeds micros"
        );
        // Leading '-' is accepted and stored as negative micros, not rejected.
        assert_eq!(parse_usd_decimal("-1.5").unwrap(), -1_500_000);
    }

    #[test]
    fn format_usd_negative_keeps_six_fraction_digits() {
        assert_eq!(format_usd(-2_500_000), "-2.500000");
    }

    #[test]
    fn format_usd_4_truncates_to_four_fraction_digits() {
        assert_eq!(format_usd_4(5_000_000), "5.0000");
        assert_eq!(format_usd_4(65_000), "0.0650");
    }

    #[test]
    fn estimate_cost_micros_rounds_each_side_down() {
        // 1 char * 1_500_000 micros/1M = 1.5 → 1; 2 chars * 2_500_000 = 5 exactly.
        // Per-side truncating division drops the 0.5 before the sides are added.
        assert_eq!(estimate_cost_micros(1, 2, 1_500_000, 2_500_000), 6);
    }
}
