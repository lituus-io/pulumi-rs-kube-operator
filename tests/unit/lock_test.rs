use std::time::Duration;

use pulumi_kubernetes_operator::operator::lock::{LockAction, LockState};

#[test]
fn new_lock_state_has_no_conflict() {
    let _state = LockState::new(Duration::from_secs(600));
    // Verifies construction doesn't panic
}

#[test]
fn first_conflict_returns_retry_with_30s() {
    let mut state = LockState::new(Duration::from_secs(600));
    match state.on_conflict() {
        LockAction::RetryAfter(d) => {
            assert_eq!(d, Duration::from_millis(30_000));
        }
        other => panic!(
            "expected RetryAfter, got {:?}",
            match other {
                LockAction::ForceUnlock => "ForceUnlock",
                LockAction::Clear => "Clear",
                _ => "RetryAfter",
            }
        ),
    }
}

#[test]
fn exponential_backoff_progression() {
    let mut state = LockState::new(Duration::from_secs(86400));

    // attempt 1: 10_000 * 3^1 = 30_000
    match state.on_conflict() {
        LockAction::RetryAfter(d) => assert_eq!(d.as_millis(), 30_000),
        _ => panic!("expected RetryAfter"),
    }
    // attempt 2: 10_000 * 3^2 = 90_000
    match state.on_conflict() {
        LockAction::RetryAfter(d) => assert_eq!(d.as_millis(), 90_000),
        _ => panic!("expected RetryAfter"),
    }
    // attempt 3: 10_000 * 3^3 = 270_000
    match state.on_conflict() {
        LockAction::RetryAfter(d) => assert_eq!(d.as_millis(), 270_000),
        _ => panic!("expected RetryAfter"),
    }
}

#[test]
fn success_resets_everything() {
    let mut state = LockState::new(Duration::from_secs(600));
    state.on_conflict();
    state.on_conflict();
    state.on_success();

    // Next conflict should start fresh
    match state.on_conflict() {
        LockAction::RetryAfter(d) => {
            assert_eq!(d, Duration::from_millis(30_000));
        }
        _ => panic!("expected RetryAfter after reset"),
    }
}

#[test]
fn backoff_capped_at_timeout() {
    let timeout = Duration::from_secs(60);
    let mut state = LockState::new(timeout);

    for _ in 0..20 {
        match state.on_conflict() {
            LockAction::RetryAfter(d) => {
                assert!(
                    d <= timeout,
                    "backoff {} exceeded timeout {}",
                    d.as_millis(),
                    timeout.as_millis()
                );
            }
            LockAction::ForceUnlock => break, // also valid
            _ => {}
        }
    }
}
