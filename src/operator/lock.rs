use std::time::{Duration, Instant};

/// Per-actor lock state -- lives inside ActorState, no heap allocation.
pub struct LockState {
    first_seen: Option<Instant>,
    timeout: Duration,
    attempts: u32,
}

pub enum LockAction {
    RetryAfter(Duration),
    ForceUnlock,
    Clear,
}

impl LockState {
    pub fn new(timeout: Duration) -> Self {
        Self {
            first_seen: None,
            timeout,
            attempts: 0,
        }
    }

    /// Called when agent returns 409 UpdateConflict.
    pub fn on_conflict(&mut self) -> LockAction {
        let now = Instant::now();
        let first = *self.first_seen.get_or_insert(now);
        self.attempts += 1;
        if now.duration_since(first) >= self.timeout {
            LockAction::ForceUnlock
        } else {
            LockAction::RetryAfter(self.backoff())
        }
    }

    pub fn on_success(&mut self) {
        self.first_seen = None;
        self.attempts = 0;
    }

    /// Update the force-unlock timeout (from Stack spec).
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Exponential backoff: 10s * 3^attempts, capped at timeout.
    /// Pure arithmetic, zero allocation.
    fn backoff(&self) -> Duration {
        let base_ms: u64 = 10_000;
        let factor: u64 = 3;
        let max_ms = self.timeout.as_millis() as u64;
        let delay = base_ms.saturating_mul(factor.saturating_pow(self.attempts.min(20)));
        Duration::from_millis(delay.min(max_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_conflict_returns_retry() {
        let mut state = LockState::new(Duration::from_secs(600));
        match state.on_conflict() {
            LockAction::RetryAfter(d) => {
                assert_eq!(d, Duration::from_millis(30_000)); // 10_000 * 3^1
            }
            _ => panic!("expected RetryAfter"),
        }
    }

    #[test]
    fn success_resets_state() {
        let mut state = LockState::new(Duration::from_secs(600));
        state.on_conflict();
        state.on_success();
        assert!(state.first_seen.is_none());
        assert_eq!(state.attempts, 0);
    }

    #[test]
    fn backoff_is_capped_at_timeout() {
        let mut state = LockState::new(Duration::from_secs(60));
        for _ in 0..30 {
            state.on_conflict();
        }
        match state.on_conflict() {
            LockAction::ForceUnlock => {
                // After enough time, should force unlock
            }
            LockAction::RetryAfter(d) => {
                assert!(d <= Duration::from_secs(60));
            }
            _ => {}
        }
    }

    #[test]
    fn backoff_increases_exponentially() {
        let mut state = LockState::new(Duration::from_secs(86400));
        // First conflict: attempt=1, backoff = 10_000 * 3^1 = 30_000ms
        match state.on_conflict() {
            LockAction::RetryAfter(d) => assert_eq!(d.as_millis(), 30_000),
            _ => panic!("expected RetryAfter"),
        }
        // Second conflict: attempt=2, backoff = 10_000 * 3^2 = 90_000ms
        match state.on_conflict() {
            LockAction::RetryAfter(d) => assert_eq!(d.as_millis(), 90_000),
            _ => panic!("expected RetryAfter"),
        }
    }
}
