//! Small shared helpers.

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// Current UTC time as RFC3339. Every timestamp column in the schema uses this
/// format so string comparison equals chronological comparison.
pub fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"))
}

/// RFC3339 timestamp `secs` seconds in the future, for cache expiry.
pub fn iso_in(secs: i64) -> String {
    (OffsetDateTime::now_utc() + time::Duration::seconds(secs))
        .format(&Rfc3339)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"))
}

/// Extract a 4-digit year from the partial dates these APIs return: `1972`,
/// `1972-05`, `1972-05-04` all yield `1972`.
pub fn year_from_partial_date(s: &str) -> Option<i32> {
    let head: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if head.len() != 4 {
        return None;
    }
    let y: i32 = head.parse().ok()?;
    // Guard against nonsense dates from bad upstream data.
    (1860..=2100).contains(&y).then_some(y)
}

/// Decade containing `year`: 1972 -> 1970.
pub fn decade_of(year: i32) -> i32 {
    year - year.rem_euclid(10)
}

/// SHA-256 hex digest, used as the `api_cache` key for a URL.
pub fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut acc, b| {
            use std::fmt::Write;
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_partial_dates() {
        assert_eq!(year_from_partial_date("1972"), Some(1972));
        assert_eq!(year_from_partial_date("1972-05"), Some(1972));
        assert_eq!(year_from_partial_date("1972-05-04"), Some(1972));
        assert_eq!(year_from_partial_date(""), None);
        assert_eq!(year_from_partial_date("197"), None);
        assert_eq!(year_from_partial_date("nonsense"), None);
        // Out of plausible range.
        assert_eq!(year_from_partial_date("0001-01-01"), None);
    }

    #[test]
    fn computes_decades() {
        assert_eq!(decade_of(1972), 1970);
        assert_eq!(decade_of(1970), 1970);
        assert_eq!(decade_of(1979), 1970);
        assert_eq!(decade_of(2001), 2000);
    }

    #[test]
    fn hashes_stably() {
        assert_eq!(sha256_hex("abc").len(), 64);
        assert_eq!(sha256_hex("abc"), sha256_hex("abc"));
        assert_ne!(sha256_hex("abc"), sha256_hex("abd"));
    }
}
