use pulumi_kubernetes_operator::core::recovery::{recovery_action, RecoveryAction};
use pulumi_kubernetes_operator::errors::*;

#[test]
fn all_transient_errors_map_to_retry() {
    let transients = vec![
        TransientError::ConnectionFailed,
        TransientError::StatusUpdateConflict,
        TransientError::WorkspaceNotReady,
        TransientError::UpdateNotFound,
        TransientError::ArtifactNotReady,
        TransientError::PrerequisiteNotSatisfied,
        TransientError::AgentRetriable { message: "test".into() },
        TransientError::OperationTimeout,
        TransientError::KubeApi {
            reason: "test error",
        },
        TransientError::KubeApiDetailed {
            reason: "test detailed",
            source: kube::Error::Api(kube::core::ErrorResponse {
                code: 500,
                message: "test".into(),
                reason: "InternalError".into(),
                status: "Failure".into(),
            }),
        },
    ];

    for t in transients {
        let err = OperatorError::Transient(t);
        assert!(
            matches!(recovery_action(&err), RecoveryAction::RetryWithBackoff { .. }),
            "expected RetryWithBackoff for {:?}",
            err
        );
        assert!(!err.should_notify(), "transient error should not notify");
    }
}

#[test]
fn all_permanent_errors_map_to_stall() {
    let permanents = vec![
        PermanentError::SpecInvalid { field: "test" },
        PermanentError::SourceUnavailable,
        PermanentError::PulumiVersionTooLow,
        PermanentError::ArtifactBuildFailed { message: "test".into() },
        PermanentError::InvalidAccessToken,
        PermanentError::UpdateFailed,
        PermanentError::ProgramNotFound,
        PermanentError::DeprecatedRefType { kind: "Env" },
        PermanentError::NamespaceIsolation,
    ];

    for p in permanents {
        let err = OperatorError::Permanent(p);
        assert!(
            matches!(recovery_action(&err), RecoveryAction::Stall),
            "expected Stall for {:?}",
            err
        );
        assert!(err.should_notify(), "permanent error should notify");
    }
}

#[test]
fn lock_errors_map_correctly() {
    let conflict = OperatorError::Lock(LockError::UpdateConflict);
    assert!(matches!(
        recovery_action(&conflict),
        RecoveryAction::ForceUnlockAndRetry
    ));

    let pending = OperatorError::Lock(LockError::PendingOperations);
    assert!(matches!(
        recovery_action(&pending),
        RecoveryAction::RetryWithBackoff { .. }
    ));
}

#[test]
fn condition_reasons_are_all_non_empty() {
    let errors: Vec<OperatorError> = vec![
        TransientError::ConnectionFailed.into(),
        PermanentError::SpecInvalid { field: "a" }.into(),
        PermanentError::SourceUnavailable.into(),
        PermanentError::PulumiVersionTooLow.into(),
        PermanentError::ArtifactBuildFailed { message: "test".into() }.into(),
        PermanentError::InvalidAccessToken.into(),
        PermanentError::UpdateFailed.into(),
        PermanentError::ProgramNotFound.into(),
        PermanentError::DeprecatedRefType { kind: "x" }.into(),
        PermanentError::NamespaceIsolation.into(),
        LockError::UpdateConflict.into(),
    ];

    for err in errors {
        let reason = err.condition_reason();
        assert!(!reason.is_empty(), "empty reason for {:?}", err);
        // Verify CamelCase (first char uppercase)
        assert!(
            reason.chars().next().unwrap().is_uppercase(),
            "reason not CamelCase: {} for {:?}",
            reason,
            err
        );
    }
}
