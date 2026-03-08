use std::time::Duration;

use pulumi_kubernetes_operator::core::lock::is_lock_error;
use pulumi_kubernetes_operator::core::time::parse_go_duration;

#[test]
fn parse_go_duration_overflow() {
    // Huge number should saturate, not panic
    let d = parse_go_duration("999999999999h");
    assert!(d.as_secs() > 0);
}

#[test]
fn parse_go_duration_empty() {
    assert_eq!(parse_go_duration(""), Duration::ZERO);
}

#[test]
fn parse_go_duration_garbage() {
    assert_eq!(parse_go_duration("abc"), Duration::ZERO);
}

#[test]
fn parse_go_duration_negative() {
    // Negative sign is treated as unknown char (resets accumulator), should not panic
    let d = parse_go_duration("-1h");
    // After '-', n resets to 0, then '1' makes n=1, 'h' adds 3600
    assert_eq!(d, Duration::from_secs(3600));
}

#[test]
fn parse_go_duration_only_numbers() {
    // No unit suffix — trailing number is not added
    assert_eq!(parse_go_duration("42"), Duration::ZERO);
}

#[test]
fn parse_go_duration_mixed_garbage() {
    // "1h abc 2m" — garbage chars reset n
    let d = parse_go_duration("1h abc 2m");
    assert_eq!(d, Duration::from_secs(3600 + 120));
}

#[test]
fn lock_detection_empty() {
    assert!(!is_lock_error(""));
}

#[test]
fn lock_detection_no_match() {
    assert!(!is_lock_error("everything is fine"));
    assert!(!is_lock_error("deployment succeeded"));
}

#[test]
fn lock_detection_positive() {
    assert!(is_lock_error("stack is currently locked by user@host"));
    assert!(is_lock_error("locked by another process"));
    assert!(is_lock_error("lock held for 5 minutes"));
    assert!(is_lock_error("update conflict detected"));
}

#[test]
fn lock_detection_case_sensitivity() {
    // Our lock detection is case-sensitive (ASCII, consistent with Pulumi output)
    assert!(!is_lock_error("CURRENTLY LOCKED"));
    assert!(!is_lock_error("Locked By"));
}
