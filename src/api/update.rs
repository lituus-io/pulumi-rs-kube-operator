use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Update CRD -- auto.pulumi.com/v1alpha1
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "auto.pulumi.com",
    version = "v1alpha1",
    kind = "Update",
    namespaced
)]
#[kube(status = "UpdateStatus", shortname = "upd")]
#[kube(
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#,
    printcolumn = r#"{"name":"Type","type":"string","jsonPath":".spec.type"}"#,
    printcolumn = r#"{"name":"Complete","type":"string","jsonPath":".status.conditions[?(@.type==\"Complete\")].status"}"#
)]
pub struct UpdateSpec {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "workspaceName"
    )]
    pub workspace_name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none", rename = "stackName")]
    pub stack_name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none", rename = "type")]
    pub update_type: Option<UpdateType>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "ttlAfterCompleted"
    )]
    pub ttl_after_completed: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel: Option<i32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "expectNoChanges"
    )]
    pub expect_no_changes: Option<bool>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replace: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target: Vec<String>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "targetDependents"
    )]
    pub target_dependents: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<bool>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "continueOnError"
    )]
    pub continue_on_error: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remove: Option<bool>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
pub struct UpdateStatus {
    #[serde(default, rename = "observedGeneration")]
    pub observed_generation: i64,

    #[serde(default, skip_serializing_if = "Option::is_none", rename = "startTime")]
    pub start_time: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none", rename = "endTime")]
    pub end_time: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permalink: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<crate::api::conditions::Condition>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub enum UpdateType {
    #[serde(rename = "preview")]
    Preview,
    #[serde(rename = "up")]
    Up,
    #[serde(rename = "destroy")]
    Destroy,
    #[serde(rename = "refresh")]
    Refresh,
}
