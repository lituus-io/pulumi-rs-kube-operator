use std::collections::BTreeMap;

// CRD types use String because kube-derive requires JsonSchema.
// String is used in internal operator types (actor state, etc).
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Stack CRD -- pulumi.com/v1
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(group = "pulumi.com", version = "v1", kind = "Stack", namespaced)]
#[kube(status = "StackStatus", shortname = "stack")]
#[kube(
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type==\"Ready\")].status"}"#
)]
pub struct StackSpec {
    /// Fully qualified stack name (org/stack). String: inlined <= 24 bytes.
    #[schemars(regex(pattern = r"^[a-zA-Z0-9._/:-]+$"))]
    pub stack: String,

    /// Backend URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,

    // --- Source (exactly one must be set -- validated in reconciler, not CRD) ---
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "projectRepo"
    )]
    pub project_repo: Option<String>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "fluxSource"
    )]
    pub flux_source: Option<FluxSource>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "programRef"
    )]
    pub program_ref: Option<ProgramReference>,

    // --- Git fields (inlined from Go's *GitSource) ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "repoDir"
    )]
    pub repo_dir: Option<String>,

    #[serde(default)]
    pub shallow: bool,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "gitAuth"
    )]
    pub git_auth: Option<GitAuthConfig>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "gitAuthSecret"
    )]
    pub git_auth_secret: Option<String>,

    // --- Config ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::api::schema::json_value_map")]
    pub config: Option<BTreeMap<String, serde_json::Value>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets: Option<BTreeMap<String, String>>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "configRef"
    )]
    pub config_ref: Option<BTreeMap<String, ConfigMapRef>>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "secretsRef"
    )]
    pub secret_refs: Option<BTreeMap<String, ResourceRef>>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "secretsProvider"
    )]
    pub secrets_provider: Option<String>,

    // --- Auth ---
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "accessTokenSecret"
    )]
    pub access_token_secret: Option<String>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "envRefs"
    )]
    pub env_refs: Option<BTreeMap<String, ResourceRef>>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub envs: Vec<String>,

    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        rename = "envSecrets"
    )]
    pub secret_envs: Vec<String>,

    // --- Lifecycle ---
    #[serde(default)]
    pub refresh: bool,

    #[serde(default, rename = "expectNoRefreshChanges")]
    pub expect_no_refresh_changes: bool,

    #[serde(default, rename = "destroyOnFinalize")]
    pub destroy_on_finalize: bool,

    #[serde(default, rename = "retryOnUpdateConflict")]
    pub retry_on_update_conflict: bool,

    #[serde(default, rename = "continueResyncOnCommitMatch")]
    pub continue_resync_on_commit_match: bool,

    #[serde(default, rename = "useLocalStackOnly")]
    pub use_local_stack_only: bool,

    #[serde(default, rename = "resyncFrequencySeconds")]
    #[schemars(range(min = 0, max = 86400))]
    pub resync_frequency_seconds: i64,

    #[serde(default)]
    pub preview: bool,

    // --- Targets ---
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,

    #[serde(default, rename = "targetDependents")]
    pub target_dependents: bool,

    // --- Prerequisites ---
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prerequisites: Vec<PrerequisiteRef>,

    // --- Workspace ---
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "serviceAccountName"
    )]
    pub service_account_name: Option<String>,

    /// RawValue for zero-copy SSA merge
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "workspaceTemplate"
    )]
    #[schemars(schema_with = "crate::api::schema::json_value_opt")]
    pub workspace_template: Option<serde_json::Value>,

    #[serde(default, rename = "workspaceReclaimPolicy")]
    pub workspace_reclaim_policy: WorkspaceReclaimPolicy,

    // --- ESC environments ---
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environment: Vec<String>,

    // --- Update template ---
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "updateTemplate"
    )]
    #[schemars(schema_with = "crate::api::schema::json_value_opt")]
    pub update_template: Option<serde_json::Value>,

    // --- Retry ---
    #[serde(default, rename = "retryMaxBackoffDurationSeconds")]
    #[schemars(range(min = 0, max = 86400))]
    pub retry_max_backoff_duration_seconds: i64,

    // --- Lock & timeout controls ---
    #[serde(
        default = "default_lock_timeout",
        rename = "lockTimeoutSeconds"
    )]
    #[schemars(range(min = 60, max = 7200))]
    pub lock_timeout_seconds: i64,

    #[serde(
        default = "default_op_timeout",
        rename = "operationTimeoutSeconds"
    )]
    #[schemars(range(min = 60, max = 86400))]
    pub operation_timeout_seconds: i64,

    #[serde(
        default = "default_fin_timeout",
        rename = "finalizerTimeoutSeconds"
    )]
    #[schemars(range(min = 60, max = 86400))]
    pub finalizer_timeout_seconds: i64,

    /// Optional project verification config. When set, the operator checks if the
    /// cloud project referenced by a program variable still exists. If not, a grace
    /// period starts, after which the stack's Kustomization is deleted (or its
    /// finalizer removed) so the cluster converges without manual cleanup.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "projectVerification"
    )]
    pub project_verification: Option<ProjectVerification>,
}

fn default_lock_timeout() -> i64 {
    900
}
fn default_op_timeout() -> i64 {
    3600
}
fn default_fin_timeout() -> i64 {
    3600
}

// --- Status ---

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
pub struct StackStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::api::schema::json_value_map")]
    pub outputs: Option<BTreeMap<String, serde_json::Value>>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "lastUpdate"
    )]
    pub last_update: Option<StackUpdateState>,

    #[serde(default, rename = "observedGeneration")]
    pub observed_generation: i64,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "observedReconcileRequest"
    )]
    pub observed_reconcile_request: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<crate::api::conditions::Condition>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "currentUpdate"
    )]
    pub current_update: Option<CurrentStackUpdate>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "pendingDeletionSince"
    )]
    pub pending_deletion_since: Option<String>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "lastProjectCheck"
    )]
    pub last_project_check: Option<ProjectCheckStatus>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct StackUpdateState {
    #[serde(default)]
    pub generation: i64,

    #[serde(default, skip_serializing_if = "Option::is_none", rename = "reconcileRequest")]
    pub reconcile_request: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none", rename = "type")]
    pub update_type: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "lastAttemptedCommit"
    )]
    pub last_attempted_commit: Option<String>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "lastSuccessfulCommit"
    )]
    pub last_successful_commit: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permalink: Option<String>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "lastResyncTime"
    )]
    pub last_resync_time: Option<String>,

    #[serde(default)]
    pub failures: i64,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct CurrentStackUpdate {
    #[serde(default)]
    pub generation: i64,

    #[serde(default, skip_serializing_if = "Option::is_none", rename = "reconcileRequest")]
    pub reconcile_request: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

// --- Sub-types ---

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct FluxSource {
    #[serde(rename = "sourceRef")]
    pub source_ref: FluxSourceReference,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct FluxSourceReference {
    #[serde(rename = "apiVersion")]
    pub api_version: String,

    pub kind: String,

    pub name: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct ProgramReference {
    pub name: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct GitAuthConfig {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "accessToken"
    )]
    pub personal_access_token: Option<ResourceRef>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "sshAuth"
    )]
    pub ssh_auth: Option<SSHAuth>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "basicAuth"
    )]
    pub basic_auth: Option<BasicAuth>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct SSHAuth {
    #[serde(rename = "sshPrivateKey")]
    pub ssh_private_key: ResourceRef,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<ResourceRef>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct BasicAuth {
    #[serde(rename = "userName")]
    pub user_name: ResourceRef,

    pub password: ResourceRef,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct ResourceRef {
    #[serde(rename = "type")]
    pub selector_type: ResourceSelectorType,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "filesystem"
    )]
    pub filesystem: Option<FSSelector>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<EnvSelector>,

    #[serde(default, skip_serializing_if = "Option::is_none", rename = "secret")]
    pub secret_ref: Option<SecretSelector>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "literal"
    )]
    pub literal_ref: Option<LiteralRef>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub enum ResourceSelectorType {
    Env,
    #[serde(rename = "FS")]
    FS,
    Secret,
    Literal,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct FSSelector {
    pub path: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct EnvSelector {
    pub name: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct SecretSelector {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    pub name: String,

    pub key: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct LiteralRef {
    pub value: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct ConfigMapRef {
    pub name: String,

    pub key: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<bool>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct PrerequisiteRef {
    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirement: Option<RequirementSpec>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct RequirementSpec {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "succeededWithinDuration"
    )]
    pub succeeded_within_duration: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
pub enum WorkspaceReclaimPolicy {
    #[default]
    Retain,
    Delete,
}

// --- Project Verification ---

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct ProjectVerification {
    /// Name of the Pulumi program variable that holds the cloud project ID.
    #[serde(rename = "variableName")]
    pub variable_name: String,

    /// Cloud provider to check against (currently only "gcp").
    #[serde(default = "default_provider")]
    pub provider: String,

    /// Days to wait after project disappears before taking action.
    #[serde(default = "default_grace_days", rename = "gracePeriodDays")]
    pub grace_period_days: i64,

    /// Action to take when the grace period expires.
    #[serde(default, rename = "onGracePeriodExpired")]
    pub on_grace_period_expired: GracePeriodAction,

    /// Optional secret with cloud credentials for the project check.
    /// If absent, uses workload identity / ADC.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "credentialSecret"
    )]
    pub credential_secret: Option<SecretSelector>,
}

fn default_provider() -> String {
    "gcp".to_owned()
}

fn default_grace_days() -> i64 {
    30
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
pub enum GracePeriodAction {
    /// Delete the Flux Kustomization that owns this Stack, triggering cascade cleanup.
    #[default]
    DeleteKustomization,
    /// Remove the Stack finalizer directly, allowing garbage collection.
    RemoveFinalizer,
}

/// Status of the last project existence check.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct ProjectCheckStatus {
    /// RFC 3339 timestamp of the check.
    #[serde(rename = "checkedAt")]
    pub checked_at: String,

    /// Result: "active", "not_found", "error", or "not_configured".
    pub result: String,

    /// Error message if result is "error".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
