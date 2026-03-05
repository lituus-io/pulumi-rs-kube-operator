use pulumi_kubernetes_operator::errors::{
    LockError, OperatorError, PermanentError, TransientError,
};

#[test]
fn transient_error_should_not_notify() {
    let cases = vec![
        OperatorError::Transient(TransientError::ConnectionFailed),
        OperatorError::Transient(TransientError::StatusUpdateConflict),
        OperatorError::Transient(TransientError::WorkspaceNotReady),
        OperatorError::Transient(TransientError::UpdateNotFound),
        OperatorError::Transient(TransientError::ArtifactNotReady),
        OperatorError::Transient(TransientError::PrerequisiteNotSatisfied),
        OperatorError::Transient(TransientError::AgentRetriable { message: "test".into() }),
        OperatorError::Transient(TransientError::OperationTimeout),
        OperatorError::Transient(TransientError::KubeApi {
            reason: "test",
        }),
        OperatorError::Transient(TransientError::KubeApiDetailed {
            reason: "test detailed",
            source: kube::Error::Api(kube::core::ErrorResponse {
                code: 500,
                message: "test".into(),
                reason: "InternalError".into(),
                status: "Failure".into(),
            }),
        }),
    ];

    for err in &cases {
        assert!(
            !err.should_notify(),
            "transient error {:?} should NOT notify Flux",
            err
        );
    }
}

#[test]
fn permanent_error_should_notify() {
    let cases = vec![
        OperatorError::Permanent(PermanentError::SpecInvalid { field: "stack" }),
        OperatorError::Permanent(PermanentError::SourceUnavailable),
        OperatorError::Permanent(PermanentError::PulumiVersionTooLow),
        OperatorError::Permanent(PermanentError::ArtifactBuildFailed { message: "test".into() }),
        OperatorError::Permanent(PermanentError::InvalidAccessToken),
        OperatorError::Permanent(PermanentError::UpdateFailed),
        OperatorError::Permanent(PermanentError::ProgramNotFound),
        OperatorError::Permanent(PermanentError::DeprecatedRefType { kind: "Env" }),
        OperatorError::Permanent(PermanentError::NamespaceIsolation),
    ];

    for err in &cases {
        assert!(
            err.should_notify(),
            "permanent error {:?} should notify Flux",
            err
        );
    }
}

#[test]
fn lock_error_should_not_notify() {
    let cases = vec![
        OperatorError::Lock(LockError::UpdateConflict),
        OperatorError::Lock(LockError::PendingOperations),
    ];

    for err in &cases {
        assert!(
            !err.should_notify(),
            "lock error {:?} should NOT notify Flux",
            err
        );
    }
}

#[test]
fn should_notify_matches_permanent_only() {
    // Exhaustive: exactly the permanent variant family triggers notification.
    let transient = OperatorError::Transient(TransientError::ConnectionFailed);
    let permanent = OperatorError::Permanent(PermanentError::UpdateFailed);
    let lock = OperatorError::Lock(LockError::UpdateConflict);

    assert!(!transient.should_notify());
    assert!(permanent.should_notify());
    assert!(!lock.should_notify());
}
