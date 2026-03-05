use std::collections::BTreeMap;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Workspace CRD -- auto.pulumi.com/v1alpha1
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "auto.pulumi.com",
    version = "v1alpha1",
    kind = "Workspace",
    namespaced
)]
#[kube(status = "WorkspaceStatus", shortname = "ws")]
#[kube(
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.conditions[?(@.type==\"Ready\")].status"}"#
)]
pub struct WorkspaceSpec {
    #[serde(
        default = "default_service_account",
        skip_serializing_if = "Option::is_none",
        rename = "serviceAccountName"
    )]
    pub service_account_name: Option<String>,

    #[serde(
        default = "default_security_profile",
        rename = "securityProfile"
    )]
    pub security_profile: SecurityProfile,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "imagePullPolicy"
    )]
    pub image_pull_policy: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<WorkspaceGitSource>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flux: Option<WorkspaceFluxSource>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<LocalSource>,

    #[serde(default, skip_serializing_if = "Vec::is_empty", rename = "envFrom")]
    #[schemars(schema_with = "crate::api::schema::json_value_vec")]
    pub env_from: Vec<serde_json::Value>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(schema_with = "crate::api::schema::json_value_vec")]
    pub env: Vec<serde_json::Value>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::api::schema::json_value_opt")]
    pub resources: Option<serde_json::Value>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "podTemplate"
    )]
    pub pod_template: Option<EmbeddedPodTemplateSpec>,

    #[serde(default, rename = "pulumiLogLevel")]
    pub pulumi_log_verbosity: u32,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stacks: Vec<WorkspaceStack>,
}

fn default_service_account() -> Option<String> {
    Some("default".to_owned())
}

fn default_security_profile() -> SecurityProfile {
    SecurityProfile::Restricted
}

// --- Status ---

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
pub struct WorkspaceStatus {
    #[serde(default, rename = "observedGeneration")]
    pub observed_generation: i64,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "pulumiVersion"
    )]
    pub pulumi_version: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<crate::api::conditions::Condition>,
}

// --- Sub-types ---

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
pub enum SecurityProfile {
    #[serde(rename = "baseline")]
    Baseline,
    #[default]
    #[serde(rename = "restricted")]
    Restricted,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct WorkspaceGitSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none", rename = "ref")]
    pub git_ref: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<WorkspaceGitAuth>,

    #[serde(default)]
    pub shallow: bool,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct WorkspaceGitAuth {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "sshPrivateKey"
    )]
    pub ssh_private_key: Option<SecretKeySelector>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<SecretKeySelector>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<SecretKeySelector>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<SecretKeySelector>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct SecretKeySelector {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct WorkspaceFluxSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct LocalSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct WorkspaceStack {
    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create: Option<bool>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "secretsProvider"
    )]
    pub secrets_provider: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config: Vec<ConfigItem>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environment: Vec<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct ConfigItem {
    pub key: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::api::schema::json_value_opt")]
    pub value: Option<serde_json::Value>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "valueFrom"
    )]
    pub value_from: Option<ConfigValueFrom>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<bool>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct ConfigValueFrom {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<bool>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct EmbeddedPodTemplateSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<EmbeddedObjectMeta>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::api::schema::json_value_opt")]
    pub spec: Option<serde_json::Value>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct EmbeddedObjectMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<BTreeMap<String, String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<BTreeMap<String, String>>,
}
