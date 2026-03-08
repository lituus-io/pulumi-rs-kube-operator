use kube::{Api, Resource, ResourceExt};

use crate::api::stack::Stack;
use crate::api::update::{Update, UpdateSpec, UpdateType};
use crate::errors::OperatorError;
use crate::operator::actors::actor::ActorState;
use crate::operator::actors::messages::NameKey;
use crate::operator::controllers::update::{check_update, UpdateCheckResult};
use crate::operator::finalizers::{stack_finalizer_action, StackFinalizerAction};
use crate::operator::manager::Manager;
use crate::operator::reconcile::source::SourceKind;
use crate::operator::reconcile::sync::is_synced;

/// Action returned by the reconciliation pipeline.
#[derive(Debug)]
pub enum ReconcileAction {
    /// Add the stack finalizer.
    AddFinalizer,
    /// Stack already finalized (no finalizer + deleting).
    Done,
    /// Remove finalizer immediately (no destroy needed).
    RemoveFinalizer,
    /// Wait for current update to complete.
    WaitForUpdate { name: String },
    /// Stack is synced, nothing to do.
    Synced,
    /// Wait for workspace to become ready.
    WaitForWorkspace,
    /// Update created, wait for it to complete.
    UpdateCreated { name: String },

    // --- Destroy flow (multi-reconcile state machine) ---
    /// Destroy Update has been created, waiting for completion.
    DestroyStarted { name: String },
    /// Destroy Update completed but failed. Stack marked failed, will retry.
    DestroyFailed { name: String, failures: i64 },
    /// Destroy completed successfully. Remove finalizer now.
    RemoveFinalizerAfterDestroy,
    // --- Project verification flow ---
    /// Project not found — start or continue grace period.
    ProjectNotFound { project_id: String },
    /// Project was found again — clear pending deletion.
    ProjectReinstated,
    /// Grace period expired — take configured action.
    ProjectTtlExpired,

    /// Current update succeeded. Update status.
    UpdateSucceeded {
        name: String,
        permalink: Option<String>,
        outputs: Option<String>,
    },
    /// Current update failed. Update status.
    UpdateFailed { name: String, message: String },
}

/// Run the reconciliation pipeline.
/// Matches the Go operator's exact 12-step sequence across multiple reconcile cycles.
pub async fn run_pipeline(
    mgr: &Manager,
    actor_state: &mut ActorState,
    key: &NameKey,
    stack: &Stack,
) -> Result<ReconcileAction, OperatorError> {
    let ns = key.ns.as_str();

    // Step 0: Finalizer check
    match stack_finalizer_action(stack) {
        StackFinalizerAction::Add => return Ok(ReconcileAction::AddFinalizer),
        StackFinalizerAction::AlreadyFinalized => return Ok(ReconcileAction::Done),
        StackFinalizerAction::RemoveImmediately => return Ok(ReconcileAction::RemoveFinalizer),
        StackFinalizerAction::RunDestroy => {
            // Multi-reconcile destroy flow
            return handle_destroy(mgr, actor_state, key, stack).await;
        }
        StackFinalizerAction::None => { /* continue normal flow */ }
    }

    // Step 0.5: Project verification (if configured)
    {
        use crate::operator::reconcile::project::{
            check_project, is_grace_period_expired, ProjectCheckResult,
        };
        match check_project(mgr, ns, stack).await {
            ProjectCheckResult::NotConfigured => { /* no-op, continue */ }
            ProjectCheckResult::Active => {
                // If there was a pending deletion, project is back — reinstate
                if stack
                    .status
                    .as_ref()
                    .and_then(|s| s.pending_deletion_since.as_ref())
                    .is_some()
                {
                    return Ok(ReconcileAction::ProjectReinstated);
                }
            }
            ProjectCheckResult::NotFound { project_id } => {
                // Check if grace period already expired
                if let Some(pending_since) = stack
                    .status
                    .as_ref()
                    .and_then(|s| s.pending_deletion_since.as_deref())
                {
                    let grace_days = stack
                        .spec
                        .project_verification
                        .as_ref()
                        .map(|v| v.grace_period_days)
                        .unwrap_or(30);
                    if is_grace_period_expired(pending_since, grace_days) {
                        return Ok(ReconcileAction::ProjectTtlExpired);
                    }
                }
                // Start or continue grace period
                return Ok(ReconcileAction::ProjectNotFound { project_id });
            }
            ProjectCheckResult::Error { message } => {
                tracing::warn!(
                    key = %key,
                    error = %message,
                    "project verification check failed, continuing normal flow"
                );
                // Don't block reconciliation on transient check errors
            }
        }
    }

    // Step 1: Check outstanding Update
    if let Some(current) = stack
        .status
        .as_ref()
        .and_then(|s| s.current_update.as_ref())
    {
        if let Some(ref name) = current.name {
            match check_update(mgr, ns, name).await? {
                UpdateCheckResult::InProgress => {
                    return Ok(ReconcileAction::WaitForUpdate {
                        name: name.to_owned(),
                    });
                }
                UpdateCheckResult::Succeeded { permalink, outputs } => {
                    return Ok(ReconcileAction::UpdateSucceeded {
                        name: name.to_owned(),
                        permalink,
                        outputs,
                    });
                }
                UpdateCheckResult::Failed { message } => {
                    return Ok(ReconcileAction::UpdateFailed {
                        name: name.to_owned(),
                        message,
                    });
                }
                UpdateCheckResult::NotFound => {
                    // Update was deleted, clear currentUpdate and continue
                    tracing::debug!(%key, update = %name, "update not found, continuing");
                }
            }
        }
    }

    // Step 2: Handle deletion (should not be reached — finalizer_action catches it)
    if stack.meta().deletion_timestamp.is_some() {
        return Ok(ReconcileAction::Done);
    }

    // Step 3: Resolve source (enum dispatch, no dyn)
    let source = SourceKind::from_spec(&stack.spec)?;
    let source_info = source.resolve(mgr, ns).await?;

    // Step 4: Protect Program CR (if programRef)
    if let SourceKind::Program { program_ref } = &source {
        ensure_program_finalizer(mgr, ns, &program_ref.name).await?;
    }

    // Step 5: isSynced check
    if is_synced(stack, &source_info.commit, actor_state) {
        return Ok(ReconcileAction::Synced);
    }

    // Step 6: Check prerequisites (synchronous Store lookups, no API round-trips)
    super::prerequisites::check_prerequisites(&mgr.stores.stacks, ns, &stack.spec.prerequisites)?;

    // Step 7: Ensure Workspace (SSA patch)
    let ws_ready = ensure_workspace(mgr, key, stack).await?;
    if !ws_ready {
        return Ok(ReconcileAction::WaitForWorkspace);
    }

    // Step 8: Create Update (with finalizer atomically)
    let update_name = create_update(mgr, key, stack, &source_info.commit).await?;

    // Step 9: Record current commit in actor state (used for lastSuccessfulCommit on success)
    actor_state.last_commit = Some(source_info.commit);

    Ok(ReconcileAction::UpdateCreated { name: update_name })
}

/// Handle the multi-reconcile destroy flow.
/// This runs when the Stack is being deleted with `destroyOnFinalize: true`.
///
/// State machine:
///  - If currentUpdate exists → check destroy Update completion
///    - Not complete → wait
///    - Complete+Failed → mark failed, clear, requeue (retry)
///    - Complete+Succeeded → mark succeeded, remove finalizer
///  - If no currentUpdate → ensure workspace, then create destroy Update
async fn handle_destroy(
    mgr: &Manager,
    _actor_state: &mut ActorState,
    key: &NameKey,
    stack: &Stack,
) -> Result<ReconcileAction, OperatorError> {
    let ns = key.ns.as_str();

    // Check if we already have a destroy Update in flight
    if let Some(current) = stack
        .status
        .as_ref()
        .and_then(|s| s.current_update.as_ref())
    {
        if let Some(ref name) = current.name {
            match check_update(mgr, ns, name).await? {
                UpdateCheckResult::InProgress => {
                    return Ok(ReconcileAction::WaitForUpdate {
                        name: name.to_owned(),
                    });
                }
                UpdateCheckResult::Succeeded { .. } => {
                    // Destroy completed successfully — remove finalizer
                    return Ok(ReconcileAction::RemoveFinalizerAfterDestroy);
                }
                UpdateCheckResult::Failed { message: _ } => {
                    let failures = stack
                        .status
                        .as_ref()
                        .and_then(|s| s.last_update.as_ref())
                        .map(|u| u.failures)
                        .unwrap_or(0);
                    return Ok(ReconcileAction::DestroyFailed {
                        name: name.to_owned(),
                        failures: failures + 1,
                    });
                }
                UpdateCheckResult::NotFound => {
                    // Destroy Update was deleted externally — recreate
                    tracing::warn!(%key, "destroy Update deleted externally, recreating");
                }
            }
        }
    }

    // No destroy Update yet (or it was deleted) — but check if we're in cooldown
    // from a recent destroy failure. Without this guard, status patches that null
    // currentUpdate trigger StackChanged watcher events that bypass the actor's
    // scheduled cooldown retry, creating a destroy storm.
    if let Some(last) = stack.status.as_ref().and_then(|s| s.last_update.as_ref()) {
        if last.state.as_deref() == Some("failed")
            && last.update_type.as_deref() == Some("destroy")
            && last.failures > 0
        {
            let elapsed = crate::core::time::elapsed_since(last.last_resync_time.as_deref());
            let cd = crate::operator::reconcile::sync::cooldown(last.failures, stack);
            if elapsed < cd {
                tracing::debug!(
                    %key,
                    failures = last.failures,
                    cooldown_secs = cd.as_secs(),
                    elapsed_secs = elapsed.as_secs(),
                    "destroy in cooldown, waiting for retry"
                );
                return Ok(ReconcileAction::WaitForUpdate {
                    name: last.name.clone().unwrap_or_default(),
                });
            }
        }
    }

    // Ensure workspace exists before creating destroy Update.
    // When Flux prunes a Kustomization, the workspace/StatefulSet/Service are
    // deleted along with the Stack. We must recreate them so `pulumi destroy`
    // has an agent to connect to.
    let ws_ready = ensure_workspace(mgr, key, stack).await?;
    if !ws_ready {
        return Ok(ReconcileAction::WaitForWorkspace);
    }

    let update_name = create_destroy_update(mgr, key, stack).await?;
    Ok(ReconcileAction::DestroyStarted { name: update_name })
}

/// Create a destroy Update CR with appropriate spec.
async fn create_destroy_update(
    mgr: &Manager,
    key: &NameKey,
    stack: &Stack,
) -> Result<String, OperatorError> {
    let ns = key.ns.as_str();
    let update_name = format!("{}-destroy-{}", key.name, chrono::Utc::now().timestamp());

    let spec = UpdateSpec {
        workspace_name: Some(key.name.to_string()),
        stack_name: Some(stack.spec.stack.to_string()),
        update_type: Some(UpdateType::Destroy),
        remove: Some(true), // Remove stack from backend after destroy
        ttl_after_completed: Some("24h".to_owned()),
        message: Some("Destroying stack (finalizer)".to_owned()),
        target: stack.spec.targets.clone(),
        target_dependents: if stack.spec.target_dependents {
            Some(true)
        } else {
            None
        },
        refresh: if stack.spec.refresh { Some(true) } else { None },
        continue_on_error: Some(true), // Best effort destroy
        parallel: None,
        expect_no_changes: None,
        replace: vec![],
    };

    // Two owner references:
    // 1. Stack with blockOwnerDeletion=true (prevents K8s from deleting Stack while Update exists)
    let stack_owner_ref = k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference {
        api_version: "pulumi.com/v1".to_owned(),
        kind: "Stack".to_owned(),
        name: stack.name_any(),
        uid: stack.uid().unwrap_or_default(),
        controller: Some(false),
        block_owner_deletion: Some(true),
    };

    let update = crate::operator::finalizers::build_update_with_finalizer(
        &update_name,
        ns,
        spec,
        stack_owner_ref,
    );

    let updates: Api<Update> = Api::namespaced(mgr.client.clone(), ns);
    updates
        .create(&kube::api::PostParams::default(), &update)
        .await
        .map_err(|e| {
            OperatorError::Transient(crate::errors::TransientError::KubeApiDetailed {
                reason: "failed to create destroy update",
                source: e,
            })
        })?;

    tracing::info!(key = %key, update = %update_name, "created destroy update");
    Ok(update_name)
}

async fn ensure_program_finalizer(
    mgr: &Manager,
    ns: &str,
    program_name: &str,
) -> Result<(), OperatorError> {
    use crate::api::conditions::{FIELD_MANAGER, PROGRAM_FINALIZER};
    use crate::api::program::Program;
    use kube::api::{Patch, PatchParams};

    let programs: Api<Program> = Api::namespaced(mgr.client.clone(), ns);
    let program = programs.get(program_name).await.map_err(|e| match &e {
        kube::Error::Api(api_err) if api_err.code == 404 => {
            OperatorError::Permanent(crate::errors::PermanentError::ProgramNotFound)
        }
        _ => OperatorError::Transient(crate::errors::TransientError::KubeApiDetailed {
            reason: "failed to get program",
            source: e,
        }),
    })?;

    if !program.finalizers().iter().any(|f| f == PROGRAM_FINALIZER) {
        tracing::debug!(program = %program_name, "adding program protection finalizer");
        let patch = serde_json::json!({
            "apiVersion": "pulumi.com/v1",
            "kind": "Program",
            "metadata": {
                "name": program_name,
                "finalizers": [PROGRAM_FINALIZER]
            }
        });
        programs
            .patch(
                program_name,
                &PatchParams::apply(FIELD_MANAGER),
                &Patch::Apply(&patch),
            )
            .await
            .map_err(|e| {
                OperatorError::Transient(crate::errors::TransientError::KubeApiDetailed {
                    reason: "failed to add program finalizer",
                    source: e,
                })
            })?;
    }

    Ok(())
}

async fn ensure_workspace(
    mgr: &Manager,
    key: &NameKey,
    stack: &Stack,
) -> Result<bool, OperatorError> {
    use crate::api::workspace::Workspace;
    use crate::operator::controllers::workspace::{
        agent_image, build_env_vars, build_headless_service, build_statefulset,
        build_workspace_spec,
    };
    use k8s_openapi::api::apps::v1::StatefulSet;
    use k8s_openapi::api::core::v1::Service;

    let ns = key.ns.as_str();
    let workspaces: Api<Workspace> = Api::namespaced(mgr.client.clone(), ns);

    // Look up the program artifact URL if this stack uses programRef
    let program_url = stack.spec.program_ref.as_ref().and_then(|pr| {
        let prog_key = kube::runtime::reflector::ObjectRef::new(&pr.name).within(ns);
        mgr.stores.programs.get(&prog_key).and_then(|prog| {
            prog.status
                .as_ref()
                .and_then(|s| s.artifact.as_ref())
                .map(|a| a.url.clone())
        })
    });

    match workspaces.get(key.name.as_str()).await {
        Ok(ws) => {
            // Workspace exists — ensure StatefulSet + Service exist too
            let image = agent_image();
            let extra_env = build_env_vars(stack);
            let owner_ref = crate::api::owner::workspace_owner_ref(&ws, true);

            // Server-Side Apply StatefulSet and Service — idempotent, no TOCTOU race
            use crate::api::conditions::FIELD_MANAGER;
            use kube::api::{Patch, PatchParams};

            let sts_api: Api<StatefulSet> = Api::namespaced(mgr.client.clone(), ns);
            let ws_resource_name = format!("{}-workspace", key.name);
            let sts = build_statefulset(
                &ws,
                key.name.as_str(),
                ns,
                owner_ref.clone(),
                &image,
                extra_env,
                program_url.as_deref(),
            );
            sts_api
                .patch(
                    &ws_resource_name,
                    &PatchParams::apply(FIELD_MANAGER),
                    &Patch::Apply(&sts),
                )
                .await
                .map_err(|e| {
                    OperatorError::Transient(crate::errors::TransientError::KubeApiDetailed {
                        reason: "failed to apply statefulset",
                        source: e,
                    })
                })?;

            let svc_api: Api<Service> = Api::namespaced(mgr.client.clone(), ns);
            let svc = build_headless_service(key.name.as_str(), ns, owner_ref);
            svc_api
                .patch(
                    &ws_resource_name,
                    &PatchParams::apply(FIELD_MANAGER),
                    &Patch::Apply(&svc),
                )
                .await
                .map_err(|e| {
                    OperatorError::Transient(crate::errors::TransientError::KubeApiDetailed {
                        reason: "failed to apply workspace service",
                        source: e,
                    })
                })?;

            // Check if workspace is ready by looking at the StatefulSet's ready replicas
            let ready = match sts_api.get(&ws_resource_name).await {
                Ok(sts) => {
                    sts.status
                        .as_ref()
                        .and_then(|s| s.ready_replicas)
                        .unwrap_or(0)
                        >= 1
                }
                Err(_) => false,
            };
            Ok(ready)
        }
        Err(kube::Error::Api(err)) if err.code == 404 => {
            // Need to create workspace
            tracing::info!(key = %key, "creating workspace");
            let ws_spec = build_workspace_spec(stack);
            let ws = Workspace::new(key.name.as_str(), ws_spec);
            let mut ws = ws;
            ws.metadata.namespace = Some(ns.to_owned());

            // Set Stack as owner of Workspace
            ws.metadata.owner_references =
                Some(vec![crate::api::owner::stack_owner_ref(stack, true)]);

            workspaces
                .create(&kube::api::PostParams::default(), &ws)
                .await
                .map_err(|e| {
                    OperatorError::Transient(crate::errors::TransientError::KubeApiDetailed {
                        reason: "failed to create workspace",
                        source: e,
                    })
                })?;
            tracing::info!(key = %key, "created workspace CR");
            Ok(false)
        }
        Err(_) => Err(OperatorError::Transient(
            crate::errors::TransientError::WorkspaceNotReady,
        )),
    }
}

async fn create_update(
    mgr: &Manager,
    key: &NameKey,
    stack: &Stack,
    commit: &str,
) -> Result<String, OperatorError> {
    let update_name = format!("{}-{}", key.name, hex_short(commit));
    let ns = key.ns.as_str();

    let update_type = if stack.spec.preview {
        UpdateType::Preview
    } else {
        UpdateType::Up
    };

    let spec = UpdateSpec {
        workspace_name: Some(key.name.to_string()),
        stack_name: Some(stack.spec.stack.to_string()),
        update_type: Some(update_type),
        parallel: None,
        message: None,
        expect_no_changes: if stack.spec.expect_no_refresh_changes {
            Some(true)
        } else {
            None
        },
        replace: vec![],
        target: stack.spec.targets.clone(),
        target_dependents: if stack.spec.target_dependents {
            Some(true)
        } else {
            None
        },
        refresh: if stack.spec.refresh { Some(true) } else { None },
        continue_on_error: None,
        remove: None,
        ttl_after_completed: None,
    };

    let owner_ref = crate::api::owner::stack_owner_ref(stack, true);

    let updates: Api<Update> = Api::namespaced(mgr.client.clone(), ns);

    let build_update =
        |s: UpdateSpec, o: k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference| {
            crate::operator::finalizers::build_update_with_finalizer(&update_name, ns, s, o)
        };

    let update = build_update(spec, owner_ref.clone());
    match updates
        .create(&kube::api::PostParams::default(), &update)
        .await
    {
        Ok(_) => {
            tracing::info!(key = %key, update = %update_name, "created update");
            Ok(update_name)
        }
        Err(kube::Error::Api(ref err)) if err.code == 409 => {
            // Update already exists — check if it's stale (completed/failed) and can be replaced
            use crate::operator::controllers::update::{check_update, UpdateCheckResult};
            match check_update(mgr, ns, &update_name).await? {
                UpdateCheckResult::InProgress => {
                    // Already running, just return the name so the pipeline waits for it
                    tracing::info!(key = %key, update = %update_name, "reusing in-progress update");
                    Ok(update_name)
                }
                UpdateCheckResult::Succeeded { .. }
                | UpdateCheckResult::Failed { .. }
                | UpdateCheckResult::NotFound => {
                    // Stale completed/failed update — remove finalizer then delete and recreate
                    tracing::info!(key = %key, update = %update_name, "deleting stale update before recreate");
                    // Remove finalizer first so the delete actually removes the object
                    let finalizer_patch = serde_json::json!({
                        "metadata": { "finalizers": null }
                    });
                    if let Err(e) = updates
                        .patch(
                            &update_name,
                            &kube::api::PatchParams::default(),
                            &kube::api::Patch::Merge(&finalizer_patch),
                        )
                        .await
                    {
                        tracing::warn!(update = %update_name, error = %e, "failed to strip finalizer from stale update");
                    }
                    if let Err(e) = updates
                        .delete(&update_name, &kube::api::DeleteParams::default())
                        .await
                    {
                        tracing::warn!(update = %update_name, error = %e, "failed to delete stale update");
                    }
                    // Brief wait for deletion to propagate
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    // Clone spec from the failed first attempt (only allocates on 409 path)
                    let retry_update = build_update(update.spec.clone(), owner_ref);
                    updates
                        .create(&kube::api::PostParams::default(), &retry_update)
                        .await
                        .map_err(|e| {
                            OperatorError::Transient(
                                crate::errors::TransientError::KubeApiDetailed {
                                    reason: "failed to recreate update after stale delete",
                                    source: e,
                                },
                            )
                        })?;
                    tracing::info!(key = %key, update = %update_name, "recreated update");
                    Ok(update_name)
                }
            }
        }
        Err(e) => Err(OperatorError::Transient(
            crate::errors::TransientError::KubeApiDetailed {
                reason: "failed to create update",
                source: e,
            },
        )),
    }
}

fn hex_short(commit: &str) -> String {
    if commit.len() >= 8 {
        commit[..8].to_owned()
    } else {
        commit.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_short_truncates_long_input() {
        assert_eq!(hex_short("abcdef1234567890"), "abcdef12");
    }

    #[test]
    fn hex_short_returns_short_input_unchanged() {
        assert_eq!(hex_short("abc"), "abc");
    }

    #[test]
    fn hex_short_empty_input() {
        assert_eq!(hex_short(""), "");
    }

    #[test]
    fn hex_short_exact_boundary() {
        assert_eq!(hex_short("12345678"), "12345678");
    }

    #[test]
    fn reconcile_action_all_variants() {
        // Verify all 12 variants are constructible and implement Debug
        let actions: Vec<ReconcileAction> = vec![
            ReconcileAction::AddFinalizer,
            ReconcileAction::Done,
            ReconcileAction::RemoveFinalizer,
            ReconcileAction::WaitForUpdate { name: "u1".into() },
            ReconcileAction::Synced,
            ReconcileAction::WaitForWorkspace,
            ReconcileAction::UpdateCreated { name: "u2".into() },
            ReconcileAction::DestroyStarted { name: "d1".into() },
            ReconcileAction::DestroyFailed {
                name: "d2".into(),
                failures: 3,
            },
            ReconcileAction::RemoveFinalizerAfterDestroy,
            ReconcileAction::UpdateSucceeded {
                name: "u3".into(),
                permalink: Some("https://app.pulumi.com/foo".into()),
                outputs: None,
            },
            ReconcileAction::UpdateFailed {
                name: "u4".into(),
                message: "boom".into(),
            },
        ];
        assert_eq!(actions.len(), 12);
        for a in &actions {
            let dbg = format!("{:?}", a);
            assert!(!dbg.is_empty());
        }
    }

    proptest::proptest! {
        #[test]
        fn hex_short_length_invariant(s in "[a-f0-9]{0,64}") {
            let result = hex_short(&s);
            proptest::prop_assert!(result.len() <= 8);
            if s.len() >= 8 {
                proptest::prop_assert_eq!(result.len(), 8);
            } else {
                proptest::prop_assert_eq!(result.len(), s.len());
            }
        }
    }
}
