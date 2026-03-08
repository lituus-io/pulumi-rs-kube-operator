use std::collections::{BTreeMap, HashMap};

use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use kube::Api;

use crate::api::conditions::{
    SECRET_OUTPUTS_ANN, UPDATE_COMPLETE, UPDATE_FAILED, UPDATE_PROGRESSING,
};
use crate::api::update::{Update, UpdateType};
use crate::core::lending::Lend;
use crate::errors::{OperatorError, TransientError};
use crate::operator::manager::Manager;
use crate::proto::agent::OutputValue;

const MAX_MESSAGE_LEN: usize = 100;

/// Check the status of an Update CR.
pub enum UpdateCheckResult {
    /// Update is still in progress.
    InProgress,
    /// Update completed successfully.
    Succeeded {
        permalink: Option<String>,
        outputs: Option<String>,
    },
    /// Update failed.
    Failed { message: String },
    /// Update not found.
    NotFound,
}

/// Check the current state of an Update.
pub async fn check_update(
    mgr: &Manager,
    ns: &str,
    name: &str,
) -> Result<UpdateCheckResult, OperatorError> {
    let updates: Api<Update> = Api::namespaced(mgr.client.clone(), ns);

    let update = match updates.get(name).await {
        Ok(u) => u,
        Err(kube::Error::Api(err)) if err.code == 404 => {
            return Ok(UpdateCheckResult::NotFound);
        }
        Err(e) => {
            return Err(OperatorError::Transient(TransientError::KubeApiDetailed {
                reason: "failed to get update",
                source: e,
            }));
        }
    };

    let status = match update.status.as_ref() {
        Some(s) => s,
        None => return Ok(UpdateCheckResult::InProgress),
    };

    // Check for Complete condition
    let complete = status
        .conditions
        .iter()
        .find(|c| c.type_ == UPDATE_COMPLETE);

    // Check for Failed condition
    let failed = status.conditions.iter().find(|c| c.type_ == UPDATE_FAILED);

    if let Some(complete_cond) = complete {
        if complete_cond.status == "True" {
            let is_failed = failed.is_some_and(|f| f.status == "True");
            if is_failed {
                let msg = failed
                    .and_then(|f| f.message.as_deref())
                    .unwrap_or("unknown error");
                return Ok(UpdateCheckResult::Failed {
                    message: truncate_message(msg),
                });
            }
            return Ok(UpdateCheckResult::Succeeded {
                permalink: status.permalink.clone(),
                outputs: status.outputs.clone(),
            });
        }
    }

    // Check if already progressing (aborted scenario)
    let progressing = status
        .conditions
        .iter()
        .find(|c| c.type_ == UPDATE_PROGRESSING);
    if let Some(prog) = progressing {
        if prog.status == "True" {
            return Ok(UpdateCheckResult::InProgress);
        }
    }

    Ok(UpdateCheckResult::InProgress)
}

/// Connect to workspace agent and stream update events.
/// Uses the connection pool for zero-allocation connection reuse.
/// Matches the Go operator's workspace + update controller sequence:
///   Install → SelectStack → SetAllConfig → Up/Destroy/Preview/Refresh
pub async fn stream_update(
    mgr: &Manager,
    _ns: &str,
    workspace_address: &str,
    update: &Update,
    config: Option<&BTreeMap<String, serde_json::Value>>,
) -> Result<StreamResult, OperatorError> {
    let guard = mgr.pool.lend(workspace_address).await?;
    let channel = guard.channel();

    let mut client = crate::proto::agent::automation_service_client::AutomationServiceClient::new(
        channel.clone(),
    )
    .max_decoding_message_size(16 * 1024 * 1024) // 16 MiB
    .max_encoding_message_size(4 * 1024 * 1024); // 4 MiB

    let spec = &update.spec;
    let mut result = StreamResult::default();

    // Step 1: Install packages/plugins required by the program.
    // Non-fatal: YAML programs with standard providers may not need explicit install
    // since providers are auto-resolved from resource type tokens during up/destroy.
    tracing::info!("installing project dependencies");
    if let Err(e) = client.install(crate::proto::agent::InstallRequest {}).await {
        tracing::warn!(error = %e, "install failed (continuing — providers may auto-resolve)");
    }

    // Step 2: Select (or create) the Pulumi stack — matches Go operator.
    // Retries on transient lock errors: concurrent stacks sharing a GCS backend
    // contend on the project-level lock during SelectStack --create.
    if let Some(ref stack_name) = spec.stack_name {
        tracing::info!(stack = %stack_name, "selecting stack");
        let retry_client = client.clone();
        crate::core::lock::retry_on_lock("SelectStack", 10, || {
            let mut c = retry_client.clone();
            let req = crate::proto::agent::SelectStackRequest {
                stack_name: stack_name.clone(),
                create: Some(true),
                secrets_provider: None,
            };
            async move { c.select_stack(req).await }
        })
        .await
        .map_err(map_grpc_error)?;
    }

    // Step 3: Set stack config if provided.
    // Also retry on transient backend lock (same GCS project-level lock).
    if let Some(cfg) = config {
        if !cfg.is_empty() {
            tracing::info!(count = cfg.len(), "setting stack config");
            let config_items: Vec<crate::proto::agent::ConfigItem> = cfg
                .iter()
                .map(|(key, value)| {
                    let val_str = match value {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    crate::proto::agent::ConfigItem {
                        key: key.clone(),
                        v: Some(crate::proto::agent::config_item::V::Value(
                            prost_types::Value {
                                kind: Some(prost_types::value::Kind::StringValue(val_str)),
                            },
                        )),
                        secret: None,
                        path: None,
                    }
                })
                .collect();

            let retry_client = client.clone();
            crate::core::lock::retry_on_lock("SetAllConfig", 10, || {
                let mut c = retry_client.clone();
                let req = crate::proto::agent::SetAllConfigRequest {
                    config: config_items.clone(),
                };
                async move { c.set_all_config(req).await }
            })
            .await
            .map_err(map_grpc_error)?;
        }
    }

    // Step 4: Run the update operation.
    match spec.update_type.as_ref() {
        Some(UpdateType::Up) => {
            let request = crate::proto::agent::UpRequest {
                parallel: spec.parallel,
                message: spec.message.clone(),
                expect_no_changes: spec.expect_no_changes,
                replace: spec.replace.clone(),
                target: spec.target.clone(),
                target_dependents: spec.target_dependents,
                policy_pack: vec![],
                refresh: spec.refresh,
                continue_on_error: spec.continue_on_error,
            };

            let mut stream = client
                .up(request)
                .await
                .map_err(map_grpc_error)?
                .into_inner();

            while let Some(msg) = futures::StreamExt::next(&mut stream).await {
                match msg {
                    Ok(frame) => {
                        if let Some(resp) = frame.response {
                            match resp {
                                crate::proto::agent::up_stream::Response::Event(_evt) => {
                                    tracing::trace!("received engine event");
                                }
                                crate::proto::agent::up_stream::Response::Result(res) => {
                                    result.permalink = res.permalink;
                                    result.stdout = res.stdout;
                                    result.stderr = res.stderr;
                                    // Collect outputs
                                    result.outputs = res.outputs;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        result.error = Some(truncate_message(e.message()));
                        return Ok(result);
                    }
                }
            }
        }
        Some(UpdateType::Preview) => {
            let request = crate::proto::agent::PreviewRequest {
                parallel: spec.parallel,
                message: spec.message.clone(),
                expect_no_changes: spec.expect_no_changes,
                replace: spec.replace.clone(),
                target: spec.target.clone(),
                target_dependents: spec.target_dependents,
                policy_pack: vec![],
                refresh: spec.refresh,
            };

            let mut stream = client
                .preview(request)
                .await
                .map_err(map_grpc_error)?
                .into_inner();

            while let Some(msg) = futures::StreamExt::next(&mut stream).await {
                match msg {
                    Ok(frame) => {
                        if let Some(resp) = frame.response {
                            match resp {
                                crate::proto::agent::preview_stream::Response::Event(_) => {
                                    tracing::trace!("received preview event");
                                }
                                crate::proto::agent::preview_stream::Response::Result(res) => {
                                    result.permalink = res.permalink;
                                    result.stdout = res.stdout;
                                    result.stderr = res.stderr;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        result.error = Some(truncate_message(e.message()));
                        return Ok(result);
                    }
                }
            }
        }
        Some(UpdateType::Destroy) => {
            let request = crate::proto::agent::DestroyRequest {
                parallel: spec.parallel,
                message: spec.message.clone(),
                target: spec.target.clone(),
                target_dependents: spec.target_dependents,
                refresh: spec.refresh,
                continue_on_error: spec.continue_on_error,
                remove: spec.remove,
            };

            let mut stream = client
                .destroy(request)
                .await
                .map_err(map_grpc_error)?
                .into_inner();

            while let Some(msg) = futures::StreamExt::next(&mut stream).await {
                match msg {
                    Ok(frame) => {
                        if let Some(resp) = frame.response {
                            match resp {
                                crate::proto::agent::destroy_stream::Response::Event(_) => {
                                    tracing::trace!("received destroy event");
                                }
                                crate::proto::agent::destroy_stream::Response::Result(res) => {
                                    result.permalink = res.permalink;
                                    result.stdout = res.stdout;
                                    result.stderr = res.stderr;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        result.error = Some(truncate_message(e.message()));
                        return Ok(result);
                    }
                }
            }
        }
        Some(UpdateType::Refresh) => {
            let request = crate::proto::agent::RefreshRequest {
                parallel: spec.parallel,
                message: spec.message.clone(),
                expect_no_changes: spec.expect_no_changes,
                target: spec.target.clone(),
            };

            let mut stream = client
                .refresh(request)
                .await
                .map_err(map_grpc_error)?
                .into_inner();

            while let Some(msg) = futures::StreamExt::next(&mut stream).await {
                match msg {
                    Ok(frame) => {
                        if let Some(resp) = frame.response {
                            match resp {
                                crate::proto::agent::refresh_stream::Response::Event(_) => {
                                    tracing::trace!("received refresh event");
                                }
                                crate::proto::agent::refresh_stream::Response::Result(res) => {
                                    result.permalink = res.permalink;
                                    result.stdout = res.stdout;
                                    result.stderr = res.stderr;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        result.error = Some(truncate_message(e.message()));
                        return Ok(result);
                    }
                }
            }
        }
        None => {
            return Err(OperatorError::Permanent(
                crate::errors::PermanentError::SpecInvalid { field: "type" },
            ));
        }
    }

    Ok(result)
}

/// Result from streaming an update.
#[derive(Default)]
pub struct StreamResult {
    pub permalink: Option<String>,
    pub stdout: String,
    pub stderr: String,
    pub outputs: HashMap<String, OutputValue>,
    pub error: Option<String>,
}

/// Create the output Secret for a successful Up operation.
/// Name: `{update-name}-stack-outputs`, Immutable, with `pulumi.com/secrets` annotation.
pub fn build_output_secret(
    update_name: &str,
    ns: &str,
    outputs: &HashMap<String, OutputValue>,
    secret_keys: &[String],
    owner_ref: OwnerReference,
) -> Secret {
    let secret_name = format!("{}-stack-outputs", update_name);

    let mut string_data: BTreeMap<String, String> = BTreeMap::new();
    for (key, ov) in outputs {
        // OutputValue.value is Bytes; interpret as UTF-8 JSON string
        let json_val = String::from_utf8_lossy(&ov.value).into_owned();
        string_data.insert(key.clone(), json_val);
    }

    let mut annotations = BTreeMap::new();
    if !secret_keys.is_empty() {
        annotations.insert(SECRET_OUTPUTS_ANN.to_owned(), secret_keys.join(","));
    }

    Secret {
        metadata: ObjectMeta {
            name: Some(secret_name),
            namespace: Some(ns.to_owned()),
            annotations: Some(annotations),
            owner_references: Some(vec![owner_ref]),
            ..Default::default()
        },
        immutable: Some(true),
        string_data: Some(string_data),
        ..Default::default()
    }
}

/// Truncate a message to MAX_MESSAGE_LEN characters (matches Go: 100 chars).
pub fn truncate_message(msg: &str) -> String {
    if msg.len() <= MAX_MESSAGE_LEN {
        msg.to_owned()
    } else {
        format!("{}...", &msg[..MAX_MESSAGE_LEN - 3])
    }
}

fn map_grpc_error(status: tonic::Status) -> OperatorError {
    let msg = status.message();
    match status.code() {
        tonic::Code::Unavailable | tonic::Code::DeadlineExceeded => {
            OperatorError::Transient(TransientError::ConnectionFailed)
        }
        tonic::Code::Aborted => OperatorError::Lock(crate::errors::LockError::UpdateConflict),
        _ => {
            // Detect Pulumi lock errors (returned as Internal gRPC code)
            if crate::core::lock::is_lock_error(msg) {
                OperatorError::Lock(crate::errors::LockError::UpdateConflict)
            } else {
                OperatorError::Transient(TransientError::AgentRetriable {
                    message: format!("{:?}: {}", status.code(), msg),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_short_message() {
        let msg = "short error";
        assert_eq!(truncate_message(msg), "short error");
    }

    #[test]
    fn test_truncate_long_message() {
        let msg = "a".repeat(200);
        let truncated = truncate_message(&msg);
        assert_eq!(truncated.len(), 100);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn test_truncate_exact_boundary() {
        let msg = "a".repeat(100);
        assert_eq!(truncate_message(&msg), msg);
    }

    #[test]
    fn test_output_secret_creation() {
        let mut outputs = HashMap::new();
        outputs.insert(
            "bucketName".to_owned(),
            OutputValue {
                value: bytes::Bytes::from("\"my-bucket\""),
                secret: false,
            },
        );

        let owner_ref = OwnerReference {
            api_version: "auto.pulumi.com/v1alpha1".to_owned(),
            kind: "Update".to_owned(),
            name: "test-update".to_owned(),
            uid: "uid-1234".to_owned(),
            controller: Some(true),
            block_owner_deletion: Some(false),
        };

        let secret = build_output_secret(
            "test-update",
            "default",
            &outputs,
            &["secretKey".to_owned()],
            owner_ref,
        );

        assert_eq!(
            secret.metadata.name.as_deref(),
            Some("test-update-stack-outputs")
        );
        assert_eq!(secret.immutable, Some(true));

        let string_data = secret.string_data.as_ref().unwrap();
        assert!(string_data.contains_key("bucketName"));
        assert_eq!(string_data.get("bucketName").unwrap(), "\"my-bucket\"");

        let annotations = secret.metadata.annotations.as_ref().unwrap();
        assert_eq!(annotations.get(SECRET_OUTPUTS_ANN).unwrap(), "secretKey");
    }
}
