/// Detect lock-related error messages from Pulumi backend.
/// Zero allocation -- Pulumi emits consistent ASCII, no need for to_lowercase().
pub fn is_lock_error(message: &str) -> bool {
    message.contains("currently locked")
        || message.contains("locked by")
        || message.contains("lock held")
        || message.contains("update conflict")
}

/// Retry a gRPC call with exponential backoff when a lock error is encountered.
/// Returns the inner response on success, or the last error on exhaustion.
/// Non-lock errors are returned immediately.
pub async fn retry_on_lock<F, Fut, T>(
    operation: &str,
    max_attempts: u32,
    mut f: F,
) -> Result<T, tonic::Status>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<tonic::Response<T>, tonic::Status>>,
{
    let mut last_err = None;
    for attempt in 0..max_attempts {
        match f().await {
            Ok(resp) => {
                if attempt > 0 {
                    tracing::info!(%operation, attempt, "succeeded after retry");
                }
                return Ok(resp.into_inner());
            }
            Err(e) => {
                if is_lock_error(e.message()) {
                    let jitter = (attempt as u64 + 1) * 2;
                    let delay = std::time::Duration::from_secs(jitter.min(20));
                    tracing::info!(
                        %operation,
                        attempt,
                        delay_secs = delay.as_secs(),
                        "locked, retrying"
                    );
                    tokio::time::sleep(delay).await;
                    last_err = Some(e);
                } else {
                    return Err(e);
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        tonic::Status::unavailable(format!(
            "{} failed after {} attempts due to lock contention",
            operation, max_attempts
        ))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_currently_locked() {
        assert!(is_lock_error("the stack is currently locked by ..."));
    }

    #[test]
    fn detects_locked_by() {
        assert!(is_lock_error("resource locked by another process"));
    }

    #[test]
    fn detects_lock_held() {
        assert!(is_lock_error("cannot update: lock held"));
    }

    #[test]
    fn detects_update_conflict() {
        assert!(is_lock_error("update conflict on stack"));
    }

    #[test]
    fn no_false_positive() {
        assert!(!is_lock_error("stack update succeeded"));
        assert!(!is_lock_error("connection failed"));
        assert!(!is_lock_error(""));
    }
}
