#[derive(Debug, thiserror::Error)]
pub enum OperatorError {
    #[error(transparent)]
    Transient(#[from] TransientError),
    #[error(transparent)]
    Permanent(#[from] PermanentError),
    #[error(transparent)]
    Lock(#[from] LockError),
}

#[derive(Debug, thiserror::Error)]
pub enum TransientError {
    #[error("connection to workspace failed")]
    ConnectionFailed,
    #[error("status update conflict")]
    StatusUpdateConflict,
    #[error("workspace not ready")]
    WorkspaceNotReady,
    #[error("update not found")]
    UpdateNotFound,
    #[error("artifact not ready")]
    ArtifactNotReady,
    #[error("prerequisite not satisfied")]
    PrerequisiteNotSatisfied,
    #[error("agent error: {message}")]
    AgentRetriable { message: String },
    #[error("operation timed out")]
    OperationTimeout,
    #[error("kube API error: {reason}")]
    KubeApi { reason: &'static str },
    #[error("kube API error: {reason}: {source}")]
    KubeApiDetailed {
        reason: &'static str,
        #[source]
        source: kube::Error,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum PermanentError {
    #[error("spec invalid: {field}")]
    SpecInvalid { field: &'static str },
    #[error("source unavailable")]
    SourceUnavailable,
    #[error("pulumi version too low")]
    PulumiVersionTooLow,
    #[error("invalid access token")]
    InvalidAccessToken,
    #[error("update failed")]
    UpdateFailed,
    #[error("program not found")]
    ProgramNotFound,
    #[error("deprecated ref type: {kind}")]
    DeprecatedRefType { kind: &'static str },
    #[error("cross-namespace ref not allowed")]
    NamespaceIsolation,
    #[error("artifact build failed: {message}")]
    ArtifactBuildFailed { message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("update conflict (lock held)")]
    UpdateConflict,
    #[error("pending operations")]
    PendingOperations,
}

impl OperatorError {
    /// Only permanent errors notify Flux -- no allocation needed.
    pub const fn should_notify(&self) -> bool {
        matches!(self, Self::Permanent(_))
    }

    /// Condition reason string -- all &'static str, zero allocation.
    pub const fn condition_reason(&self) -> &'static str {
        match self {
            Self::Permanent(e) => e.condition_reason(),
            Self::Transient(_) => "RetryingAfterFailure",
            Self::Lock(_) => "UpdateConflict",
        }
    }
}

impl PermanentError {
    pub const fn condition_reason(&self) -> &'static str {
        match self {
            Self::SpecInvalid { .. } => "SpecInvalid",
            Self::SourceUnavailable => "SourceUnavailable",
            Self::PulumiVersionTooLow => "PulumiVersionTooLow",
            Self::InvalidAccessToken => "InvalidAccessToken",
            Self::UpdateFailed => "UpdateFailed",
            Self::ProgramNotFound => "SourceUnavailable",
            Self::DeprecatedRefType { .. } => "SpecInvalid",
            Self::NamespaceIsolation => "SpecInvalid",
            Self::ArtifactBuildFailed { .. } => "SpecInvalid",
        }
    }
}

/// Top-level error for startup/server paths (replaces Box<dyn Error>).
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("kube client: {0}")]
    Kube(#[from] kube::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("transport: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("address parse: {0}")]
    AddrParse(#[from] std::net::AddrParseError),
    #[error("hyper: {0}")]
    Hyper(#[from] hyper::Error),
    #[error("init: {0}")]
    Init(#[from] crate::agent::init::InitError),
    #[error("controller exited unexpectedly: {0}")]
    ControllerExited(String),
    #[error("{0}")]
    Generic(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_errors_should_not_notify() {
        let err = OperatorError::Transient(TransientError::ConnectionFailed);
        assert!(!err.should_notify());
    }

    #[test]
    fn permanent_errors_should_notify() {
        let err = OperatorError::Permanent(PermanentError::SpecInvalid { field: "stack" });
        assert!(err.should_notify());
    }

    #[test]
    fn lock_errors_should_not_notify() {
        let err = OperatorError::Lock(LockError::UpdateConflict);
        assert!(!err.should_notify());
    }

    #[test]
    fn condition_reason_is_static_str() {
        let err = OperatorError::Permanent(PermanentError::SpecInvalid { field: "stack" });
        assert_eq!(err.condition_reason(), "SpecInvalid");

        let err = OperatorError::Transient(TransientError::ConnectionFailed);
        assert_eq!(err.condition_reason(), "RetryingAfterFailure");

        let err = OperatorError::Lock(LockError::UpdateConflict);
        assert_eq!(err.condition_reason(), "UpdateConflict");
    }

    #[test]
    fn error_display_messages() {
        let err = TransientError::KubeApi {
            reason: "not found",
        };
        assert_eq!(err.to_string(), "kube API error: not found");

        let err = PermanentError::DeprecatedRefType { kind: "Env" };
        assert_eq!(err.to_string(), "deprecated ref type: Env");

        let err = PermanentError::ArtifactBuildFailed {
            message: "yaml serialization failed".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "artifact build failed: yaml serialization failed"
        );
    }

    #[test]
    fn from_conversions() {
        let t: OperatorError = TransientError::ConnectionFailed.into();
        assert!(matches!(t, OperatorError::Transient(_)));

        let p: OperatorError = PermanentError::UpdateFailed.into();
        assert!(matches!(p, OperatorError::Permanent(_)));

        let l: OperatorError = LockError::UpdateConflict.into();
        assert!(matches!(l, OperatorError::Lock(_)));
    }
}
