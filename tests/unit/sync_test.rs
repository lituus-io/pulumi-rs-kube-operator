use std::time::Duration;

use pulumi_kubernetes_operator::api::stack::{Stack, StackSpec};
use pulumi_kubernetes_operator::operator::reconcile::sync::{cooldown, resync_freq};

fn default_spec() -> StackSpec {
    serde_json::from_str(r#"{"stack": "org/test"}"#).unwrap()
}

#[test]
fn cooldown_monotonically_increases() {
    let stack = Stack::new("test", default_spec());
    let mut prev = Duration::ZERO;
    for i in 0..=10 {
        let c = cooldown(i, &stack);
        assert!(c >= prev, "cooldown decreased at failures={}", i);
        prev = c;
    }
}

#[test]
fn cooldown_never_exceeds_cap() {
    let stack = Stack::new("test", default_spec());
    for i in 0..50 {
        let c = cooldown(i, &stack);
        assert!(c <= Duration::from_secs(86400), "exceeded 24h at failures={}", i);
    }
}

#[test]
fn cooldown_respects_custom_cap() {
    let mut spec = default_spec();
    spec.retry_max_backoff_duration_seconds = 120;
    let stack = Stack::new("test", spec);
    for i in 0..50 {
        let c = cooldown(i, &stack);
        assert!(c <= Duration::from_secs(120), "exceeded 120s at failures={}", i);
    }
}

#[test]
fn resync_freq_positive_only() {
    let mut spec = default_spec();
    spec.resync_frequency_seconds = -1;
    let stack = Stack::new("test", spec);
    let freq = resync_freq(&stack);
    assert!(freq > Duration::ZERO);
}
