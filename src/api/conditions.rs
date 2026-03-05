use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Kubernetes-style condition (with JsonSchema support).
/// k8s_openapi::Condition doesn't implement JsonSchema,
/// so we define our own compatible type.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct Condition {
    #[serde(rename = "type")]
    pub type_: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "lastTransitionTime"
    )]
    pub last_transition_time: Option<String>,
    #[serde(default, rename = "observedGeneration")]
    pub observed_generation: i64,
}

// Stack conditions
pub const READY: &str = "Ready";
pub const STALLED: &str = "Stalled";
pub const RECONCILING: &str = "Reconciling";

// Stack reasons (all CamelCase, all &'static str)
pub const READY_COMPLETED: &str = "ProcessingCompleted";
pub const NOT_READY_IN_PROGRESS: &str = "NotReadyInProgress";
pub const NOT_READY_STALLED: &str = "NotReadyStalled";
pub const RECONCILING_PROCESSING: &str = "StackProcessing";
pub const RECONCILING_RETRY: &str = "RetryingAfterFailure";
pub const RECONCILING_PREREQ: &str = "PrerequisiteNotSatisfied";
pub const STALLED_SPEC_INVALID: &str = "SpecInvalid";
pub const STALLED_SOURCE_UNAVAIL: &str = "SourceUnavailable";
pub const STALLED_CONFLICT: &str = "UpdateConflict";
pub const STALLED_VERSION_LOW: &str = "PulumiVersionTooLow";
pub const STALLED_WORKSPACE_FAIL: &str = "WorkspaceFailed";

// Update conditions
pub const UPDATE_COMPLETE: &str = "Complete";
pub const UPDATE_FAILED: &str = "Failed";
pub const UPDATE_PROGRESSING: &str = "Progressing";

// Finalizer names
pub const STACK_FINALIZER: &str = "finalizer.stack.pulumi.com";
pub const PROGRAM_FINALIZER: &str = "finalizer.program.pulumi.com";

// Field managers
pub const FIELD_MANAGER: &str = "pulumi-kubernetes-operator";
pub const STACK_FINALIZER_FM: &str = "pulumi-kubernetes-operator/stack-finalizer";

// Labels
pub const COMPONENT_LABEL: &str = "pulumi.com/component";
pub const STACK_NAME_LABEL: &str = "pulumi.com/stack-name";
pub const AUTO_COMPONENT_LABEL: &str = "auto.pulumi.com/component";
pub const WORKSPACE_NAME_LABEL: &str = "auto.pulumi.com/workspace-name";
pub const UPDATE_NAME_LABEL: &str = "auto.pulumi.com/update-name";

// Annotations
pub const RECONCILE_REQUEST_ANN: &str = "pulumi.com/reconciliation-request";
pub const SECRET_OUTPUTS_ANN: &str = "pulumi.com/secrets";
pub const POD_INITIALIZED_ANN: &str = "auto.pulumi.com/initialized";
pub const POD_REVISION_HASH_ANN: &str = "auto.pulumi.com/revision-hash";

// Event reasons (stack)
pub const EVT_CONFIG_INVALID: &str = "StackConfigInvalid";
pub const EVT_INIT_FAILURE: &str = "StackInitializationFailure";
pub const EVT_GIT_AUTH_FAILURE: &str = "StackGitAuthenticationFailure";
pub const EVT_UPDATE_FAILURE: &str = "StackUpdateFailure";
pub const EVT_CONFLICT_DETECTED: &str = "StackUpdateConflictDetected";
pub const EVT_OUTPUT_FAILURE: &str = "StackOutputRetrievalFailure";
pub const EVT_UPDATE_DETECTED: &str = "StackUpdateDetected";
pub const EVT_NOT_FOUND: &str = "StackNotFound";
pub const EVT_UPDATE_SUCCESS: &str = "StackCreated";
pub const EVT_DESTROY_SUCCESS: &str = "StackDestroyed";
pub const EVT_WORKSPACE_DELETED: &str = "WorkspaceDeleted";
pub const EVT_LOCK_UNLOCKED: &str = "LockForceUnlocked";

// Project verification conditions
pub const PENDING_DELETION: &str = "PendingDeletion";
pub const PENDING_DELETION_PROJECT: &str = "ProjectNotFound";
pub const PENDING_DELETION_REINSTATED: &str = "ProjectReinstated";
pub const PENDING_DELETION_TTL_EXPIRED: &str = "GracePeriodExpired";

// Event reasons (update/workspace)
pub const EVT_CONNECTION_FAILURE: &str = "ConnectionFailure";
pub const EVT_INSTALL_FAILURE: &str = "InstallationFailure";
pub const EVT_STACK_INIT_FAILURE: &str = "StackInitializationFailure";
pub const EVT_UPDATE_FAILED: &str = "UpdateFailed";
pub const EVT_INITIALIZED: &str = "Initialized";
pub const EVT_UPDATE_EXPIRED: &str = "UpdateExpired";
pub const EVT_UPDATE_SUCCEEDED: &str = "UpdateSucceeded";
