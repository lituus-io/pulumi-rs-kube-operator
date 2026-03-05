use crate::errors::OperatorError;

/// Recovery action -- compiler enforces every error maps to exactly one action.
#[derive(Debug)]
pub enum RecoveryAction {
    RetryWithBackoff { base_ms: u64, max_ms: u64 },
    Stall,
    ForceUnlockAndRetry,
}

/// Maps an operator error to its recovery action.
pub fn recovery_action(err: &OperatorError) -> RecoveryAction {
    use crate::errors::{LockError, TransientError};
    match err {
        OperatorError::Transient(t) => match t {
            TransientError::ConnectionFailed => RecoveryAction::RetryWithBackoff {
                base_ms: 5_000,
                max_ms: 60_000,
            },
            TransientError::StatusUpdateConflict => RecoveryAction::RetryWithBackoff {
                base_ms: 1_000,
                max_ms: 10_000,
            },
            TransientError::WorkspaceNotReady => RecoveryAction::RetryWithBackoff {
                base_ms: 5_000,
                max_ms: 120_000,
            },
            TransientError::UpdateNotFound => RecoveryAction::RetryWithBackoff {
                base_ms: 2_000,
                max_ms: 30_000,
            },
            TransientError::ArtifactNotReady => RecoveryAction::RetryWithBackoff {
                base_ms: 5_000,
                max_ms: 60_000,
            },
            TransientError::PrerequisiteNotSatisfied => RecoveryAction::RetryWithBackoff {
                base_ms: 10_000,
                max_ms: 300_000,
            },
            TransientError::AgentRetriable { .. } => RecoveryAction::RetryWithBackoff {
                base_ms: 5_000,
                max_ms: 60_000,
            },
            TransientError::OperationTimeout => RecoveryAction::RetryWithBackoff {
                base_ms: 30_000,
                max_ms: 300_000,
            },
            TransientError::KubeApi { .. } | TransientError::KubeApiDetailed { .. } => {
                RecoveryAction::RetryWithBackoff {
                    base_ms: 1_000,
                    max_ms: 30_000,
                }
            }
        },
        OperatorError::Permanent(_) => RecoveryAction::Stall,
        OperatorError::Lock(l) => match l {
            LockError::UpdateConflict => RecoveryAction::ForceUnlockAndRetry,
            LockError::PendingOperations => RecoveryAction::RetryWithBackoff {
                base_ms: 10_000,
                max_ms: 60_000,
            },
        },
    }
}
