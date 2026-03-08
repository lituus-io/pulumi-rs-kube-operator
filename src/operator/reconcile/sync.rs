use std::time::Duration;

use kube::{Resource, ResourceExt};

use crate::api::conditions::RECONCILE_REQUEST_ANN;
use crate::api::stack::Stack;
use crate::core::time::elapsed_since;
use crate::operator::actors::actor::ActorState;

/// Pure function: determines if stack is synced.
/// Zero allocation -- all comparisons are on borrowed strings.
pub fn is_synced(stack: &Stack, current_commit: &str, _actor_state: &mut ActorState) -> bool {
    let status = match stack.status.as_ref().and_then(|s| s.last_update.as_ref()) {
        None => return false, // Initial update needed
        Some(s) => s,
    };

    let generation = stack.meta().generation.unwrap_or(0);
    if status.generation != generation {
        return false;
    }

    // Check reconcile request annotation
    let ann_val = stack
        .annotations()
        .get(RECONCILE_REQUEST_ANN)
        .map(|s| s.as_str());
    if status.reconcile_request.as_deref() != ann_val {
        return false;
    }

    match status.state.as_deref().unwrap_or("") {
        "succeeded" => {
            if stack.meta().deletion_timestamp.is_some() {
                return true;
            }
            if status.last_successful_commit.as_deref() != Some(current_commit) {
                return false;
            }
            if !stack.spec.continue_resync_on_commit_match {
                return true;
            }
            // Check resync frequency
            let freq = resync_freq(stack);
            elapsed_since(status.last_resync_time.as_deref()) < freq
        }
        "failed" => {
            if status.last_attempted_commit.as_deref() != Some(current_commit) {
                return false;
            }
            // Inside cooldown window = synced (don't retry yet)
            elapsed_since(status.last_resync_time.as_deref()) < cooldown(status.failures, stack)
        }
        other => {
            tracing::warn!(state = ?other, "unknown lastUpdate state, forcing reconcile");
            false // Unknown state: trigger reconcile
        }
    }
}

/// Resync frequency from spec, defaulting to 60s.
pub fn resync_freq(stack: &Stack) -> Duration {
    let secs = stack.spec.resync_frequency_seconds;
    if secs > 0 {
        Duration::from_secs(secs as u64)
    } else {
        Duration::from_secs(60)
    }
}

/// Cooldown: 10s * 3^failures, capped at 24h or spec override.
/// Pure arithmetic, no allocation.
pub fn cooldown(failures: i64, stack: &Stack) -> Duration {
    if failures <= 0 {
        return Duration::ZERO;
    }
    let cap_secs = if stack.spec.retry_max_backoff_duration_seconds > 0 {
        stack.spec.retry_max_backoff_duration_seconds as u64
    } else {
        86400 // 24 hours
    };
    let base: u64 = 10;
    let factor: u64 = 3;
    let mut delay = base;
    for _ in 0..failures.min(30) {
        delay = delay.saturating_mul(factor);
    }
    Duration::from_secs(delay.min(cap_secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::stack::StackSpec;

    fn default_spec() -> StackSpec {
        serde_json::from_str(r#"{"stack": "org/test"}"#).unwrap()
    }

    #[test]
    fn cooldown_zero_for_no_failures() {
        let stack = Stack::new("test", default_spec());
        assert_eq!(cooldown(0, &stack), Duration::ZERO);
        assert_eq!(cooldown(-1, &stack), Duration::ZERO);
    }

    #[test]
    fn cooldown_increases_exponentially() {
        let stack = Stack::new("test", default_spec());
        let c1 = cooldown(1, &stack);
        let c2 = cooldown(2, &stack);
        let c3 = cooldown(3, &stack);
        assert!(c2 > c1);
        assert!(c3 > c2);
    }

    #[test]
    fn cooldown_capped_at_24h() {
        let stack = Stack::new("test", default_spec());
        let c = cooldown(100, &stack);
        assert!(c <= Duration::from_secs(86400));
    }

    #[test]
    fn cooldown_respects_spec_override() {
        let mut spec = default_spec();
        spec.retry_max_backoff_duration_seconds = 60;
        let stack = Stack::new("test", spec);
        let c = cooldown(100, &stack);
        assert!(c <= Duration::from_secs(60));
    }

    #[test]
    fn resync_freq_defaults_to_60s() {
        let stack = Stack::new("test", default_spec());
        assert_eq!(resync_freq(&stack), Duration::from_secs(60));
    }

    #[test]
    fn resync_freq_from_spec() {
        let mut spec = default_spec();
        spec.resync_frequency_seconds = 300;
        let stack = Stack::new("test", spec);
        assert_eq!(resync_freq(&stack), Duration::from_secs(300));
    }
}
