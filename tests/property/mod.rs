use proptest::prelude::*;
use std::time::Duration;

use pulumi_kubernetes_operator::agent::redact::redact_stderr;
use pulumi_kubernetes_operator::api::stack::{Stack, StackSpec};
use pulumi_kubernetes_operator::core::lock::is_lock_error;
use pulumi_kubernetes_operator::core::time::parse_go_duration;
use pulumi_kubernetes_operator::operator::lock::{LockAction, LockState};
use pulumi_kubernetes_operator::operator::reconcile::sync::cooldown;

fn default_spec() -> StackSpec {
    serde_json::from_str(r#"{"stack": "org/test"}"#).unwrap()
}

proptest! {
    /// Cooldown is monotonically non-decreasing with failure count.
    #[test]
    fn cooldown_monotonic(failures in 0i64..100) {
        let stack = Stack::new("test", default_spec());
        if failures > 0 {
            let prev = cooldown(failures - 1, &stack);
            let curr = cooldown(failures, &stack);
            prop_assert!(curr >= prev, "cooldown decreased: {} -> {} at failures={}", prev.as_secs(), curr.as_secs(), failures);
        }
    }

    /// Cooldown never exceeds the cap (24h default).
    #[test]
    fn cooldown_bounded(failures in 0i64..1000) {
        let stack = Stack::new("test", default_spec());
        let c = cooldown(failures, &stack);
        prop_assert!(c <= Duration::from_secs(86400), "cooldown exceeded 24h: {} at failures={}", c.as_secs(), failures);
    }

    /// Cooldown respects custom cap.
    #[test]
    fn cooldown_custom_cap(failures in 0i64..100, cap in 1u64..3600) {
        let mut spec = default_spec();
        spec.retry_max_backoff_duration_seconds = cap as i64;
        let stack = Stack::new("test", spec);
        let c = cooldown(failures, &stack);
        prop_assert!(c <= Duration::from_secs(cap), "cooldown exceeded custom cap: {} > {} at failures={}", c.as_secs(), cap, failures);
    }

    /// Lock backoff is always <= timeout.
    #[test]
    fn lock_backoff_bounded(timeout_secs in 1u64..3600, attempts in 1u32..30) {
        let timeout = Duration::from_secs(timeout_secs);
        let mut state = LockState::new(timeout);
        for _ in 0..attempts {
            match state.on_conflict() {
                LockAction::RetryAfter(d) => {
                    prop_assert!(d <= timeout, "backoff exceeded timeout");
                }
                LockAction::ForceUnlock => break,
                LockAction::Clear => break,
            }
        }
    }

    /// Metrics reconcile counter is monotonic: increment N times → counter == N.
    #[test]
    fn metrics_reconciles_monotonic(n in 1u64..200) {
        let metrics = pulumi_kubernetes_operator::operator::metrics::Metrics::new();
        for _ in 0..n {
            metrics.inc_reconciles();
        }
        let val = metrics.reconciles_total.load(std::sync::atomic::Ordering::Relaxed);
        prop_assert_eq!(val, n);
    }

    /// parse_go_duration never panics on any valid h/m/s combination.
    #[test]
    fn parse_go_duration_no_panic(h in 0u64..100, m in 0u64..60, s in 0u64..60) {
        let input = format!("{}h{}m{}s", h, m, s);
        let d = parse_go_duration(&input);
        let expected = Duration::from_secs(h * 3600 + m * 60 + s);
        prop_assert_eq!(d, expected);
    }

    // --- Security property tests ---

    /// Redaction never panics on arbitrary input.
    #[test]
    fn redact_stderr_no_panic(input in "\\PC{0,10000}") {
        let _ = redact_stderr(&input);
    }

    /// Redaction output never contains known secret patterns when input has them.
    #[test]
    fn redact_stderr_no_leaks(input in ".*password=[a-zA-Z0-9]{10}.*") {
        let result = redact_stderr(&input);
        prop_assert!(!result.contains("password="));
    }

    /// parse_go_duration never panics on arbitrary strings.
    #[test]
    fn parse_go_duration_arbitrary(input in "\\PC{0,100}") {
        let _ = parse_go_duration(&input);
    }

    /// is_lock_error never panics on arbitrary strings.
    #[test]
    fn lock_detection_no_panic(input in "\\PC{0,1000}") {
        let _ = is_lock_error(&input);
    }
}
