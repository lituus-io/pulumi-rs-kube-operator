use pulumi_kubernetes_operator::core::recovery::{recovery_action, RecoveryAction};
use pulumi_kubernetes_operator::errors::{
    LockError, OperatorError, PermanentError, TransientError,
};

#[test]
fn transient_connection_failed_retries_with_backoff() {
    let err = OperatorError::Transient(TransientError::ConnectionFailed);
    match recovery_action(&err) {
        RecoveryAction::RetryWithBackoff { base_ms, max_ms } => {
            assert_eq!(base_ms, 5_000);
            assert_eq!(max_ms, 60_000);
        }
        other => panic!("expected RetryWithBackoff, got {:?}", other),
    }
}

#[test]
fn transient_status_update_conflict_retries_fast() {
    let err = OperatorError::Transient(TransientError::StatusUpdateConflict);
    match recovery_action(&err) {
        RecoveryAction::RetryWithBackoff { base_ms, max_ms } => {
            assert_eq!(base_ms, 1_000);
            assert_eq!(max_ms, 10_000);
        }
        other => panic!("expected RetryWithBackoff, got {:?}", other),
    }
}

#[test]
fn transient_workspace_not_ready_retries() {
    let err = OperatorError::Transient(TransientError::WorkspaceNotReady);
    match recovery_action(&err) {
        RecoveryAction::RetryWithBackoff { base_ms, max_ms } => {
            assert_eq!(base_ms, 5_000);
            assert_eq!(max_ms, 120_000);
        }
        other => panic!("expected RetryWithBackoff, got {:?}", other),
    }
}

#[test]
fn transient_kube_api_retries() {
    let err = OperatorError::Transient(TransientError::KubeApi { reason: "test" });
    match recovery_action(&err) {
        RecoveryAction::RetryWithBackoff { base_ms, max_ms } => {
            assert_eq!(base_ms, 1_000);
            assert_eq!(max_ms, 30_000);
        }
        other => panic!("expected RetryWithBackoff, got {:?}", other),
    }
}

#[test]
fn transient_kube_api_detailed_retries() {
    let err = OperatorError::Transient(TransientError::KubeApiDetailed {
        reason: "test detailed",
        source: kube::Error::Api(kube::core::ErrorResponse {
            code: 500,
            message: "test".into(),
            reason: "InternalError".into(),
            status: "Failure".into(),
        }),
    });
    match recovery_action(&err) {
        RecoveryAction::RetryWithBackoff { base_ms, max_ms } => {
            assert_eq!(base_ms, 1_000);
            assert_eq!(max_ms, 30_000);
        }
        other => panic!("expected RetryWithBackoff, got {:?}", other),
    }
}

#[test]
fn transient_operation_timeout_retries_slowly() {
    let err = OperatorError::Transient(TransientError::OperationTimeout);
    match recovery_action(&err) {
        RecoveryAction::RetryWithBackoff { base_ms, max_ms } => {
            assert_eq!(base_ms, 30_000);
            assert_eq!(max_ms, 300_000);
        }
        other => panic!("expected RetryWithBackoff, got {:?}", other),
    }
}

#[test]
fn permanent_errors_stall() {
    let cases = vec![
        OperatorError::Permanent(PermanentError::SpecInvalid { field: "test" }),
        OperatorError::Permanent(PermanentError::SourceUnavailable),
        OperatorError::Permanent(PermanentError::PulumiVersionTooLow),
        OperatorError::Permanent(PermanentError::InvalidAccessToken),
        OperatorError::Permanent(PermanentError::UpdateFailed),
        OperatorError::Permanent(PermanentError::ProgramNotFound),
        OperatorError::Permanent(PermanentError::NamespaceIsolation),
    ];

    for err in &cases {
        assert!(
            matches!(recovery_action(err), RecoveryAction::Stall),
            "permanent error {:?} should map to Stall",
            err,
        );
    }
}

#[test]
fn lock_update_conflict_force_unlocks() {
    let err = OperatorError::Lock(LockError::UpdateConflict);
    assert!(matches!(
        recovery_action(&err),
        RecoveryAction::ForceUnlockAndRetry,
    ));
}

#[test]
fn lock_pending_operations_retries() {
    let err = OperatorError::Lock(LockError::PendingOperations);
    match recovery_action(&err) {
        RecoveryAction::RetryWithBackoff { base_ms, max_ms } => {
            assert_eq!(base_ms, 10_000);
            assert_eq!(max_ms, 60_000);
        }
        other => panic!("expected RetryWithBackoff, got {:?}", other),
    }
}
