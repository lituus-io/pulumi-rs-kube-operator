use std::time::Duration;

/// Parse an RFC3339 timestamp and return elapsed duration since then.
/// Unified implementation -- used by sync.rs and prerequisites.rs.
pub fn elapsed_since(time_str: Option<&str>) -> Duration {
    let Some(time_str) = time_str else {
        return Duration::MAX;
    };

    match chrono::DateTime::parse_from_rfc3339(time_str) {
        Ok(dt) => {
            let now = chrono::Utc::now();
            let elapsed = now.signed_duration_since(dt.with_timezone(&chrono::Utc));
            elapsed.to_std().unwrap_or(Duration::ZERO)
        }
        Err(_) => Duration::MAX,
    }
}

/// Parse a Go-style duration string (e.g., "1h", "30m", "5m30s", "1h30m5s").
/// Zero heap allocations. Uses saturating arithmetic to prevent overflow panics.
pub fn parse_go_duration(s: &str) -> Duration {
    let mut total_secs: u64 = 0;
    let mut n: u64 = 0;

    for c in s.bytes() {
        match c {
            b'0'..=b'9' => n = n.saturating_mul(10).saturating_add((c - b'0') as u64),
            b'h' => {
                total_secs = total_secs.saturating_add(n.saturating_mul(3600));
                n = 0;
            }
            b'm' => {
                total_secs = total_secs.saturating_add(n.saturating_mul(60));
                n = 0;
            }
            b's' => {
                total_secs = total_secs.saturating_add(n);
                n = 0;
            }
            _ => {
                n = 0;
            }
        }
    }

    Duration::from_secs(total_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hours() {
        assert_eq!(parse_go_duration("1h"), Duration::from_secs(3600));
    }

    #[test]
    fn parse_minutes() {
        assert_eq!(parse_go_duration("30m"), Duration::from_secs(1800));
    }

    #[test]
    fn parse_compound() {
        assert_eq!(parse_go_duration("1h30m"), Duration::from_secs(5400));
    }

    #[test]
    fn parse_seconds() {
        assert_eq!(parse_go_duration("45s"), Duration::from_secs(45));
    }

    #[test]
    fn parse_full_compound() {
        assert_eq!(parse_go_duration("1h30m5s"), Duration::from_secs(5405));
    }

    #[test]
    fn parse_overflow_saturates() {
        // Huge number of hours should saturate, not panic
        let d = parse_go_duration("99999999999999999999h");
        assert!(d.as_secs() > 0);
    }

    #[test]
    fn elapsed_since_none_is_max() {
        assert_eq!(elapsed_since(None), Duration::MAX);
    }

    #[test]
    fn elapsed_since_invalid_is_max() {
        assert_eq!(elapsed_since(Some("not-a-date")), Duration::MAX);
    }

    #[test]
    fn elapsed_since_recent_is_small() {
        let now = chrono::Utc::now().to_rfc3339();
        let elapsed = elapsed_since(Some(&now));
        assert!(elapsed < Duration::from_secs(2));
    }
}
