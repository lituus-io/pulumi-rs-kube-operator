pub mod program;
pub mod update;
pub mod workspace;

use kube::runtime::reflector;
use kube::runtime::watcher;
use kube::runtime::watcher::Config as WatcherConfig;
use kube::{Api, ResourceExt};

use futures::StreamExt;

use crate::api::program::Program;
use crate::api::stack::Stack;
use crate::api::update::Update;
use crate::api::workspace::Workspace;
use crate::operator::actors::dispatcher::Dispatcher;
use crate::operator::actors::messages::{NameKey, ReconcileTrigger, StackMessage};
use crate::operator::manager::{InformerStores, Manager};

/// Create reflector stores and writers for all CRD types.
/// Returns (stores, writers) -- stores go into Manager, writers are consumed by watchers.
pub fn create_stores() -> (
    InformerStores,
    reflector::store::Writer<Stack>,
    reflector::store::Writer<Workspace>,
    reflector::store::Writer<Update>,
    reflector::store::Writer<Program>,
) {
    let (stack_store, stack_writer) = reflector::store();
    let (ws_store, ws_writer) = reflector::store();
    let (update_store, update_writer) = reflector::store();
    let (program_store, program_writer) = reflector::store();

    let stores = InformerStores {
        stacks: stack_store,
        workspaces: ws_store,
        updates: update_store,
        programs: program_store,
    };

    (
        stores,
        stack_writer,
        ws_writer,
        update_writer,
        program_writer,
    )
}

/// Run all controllers concurrently.
pub async fn run_all(
    mgr: &'static Manager,
    dispatcher: Dispatcher,
    stack_writer: reflector::store::Writer<Stack>,
    ws_writer: reflector::store::Writer<Workspace>,
    update_writer: reflector::store::Writer<Update>,
    program_writer: reflector::store::Writer<Program>,
) -> Result<(), crate::errors::RunError> {
    let dispatcher: &'static Dispatcher = Box::leak(Box::new(dispatcher));

    // Start program file server in background
    tokio::spawn(async move {
        if let Err(e) =
            program::serve_file_server(&mgr.program_file_server, program::FILE_SERVER_PORT).await
        {
            tracing::error!(error = %e, "program file server exited with error");
        }
    });

    let exit_reason = tokio::select! {
        r = run_stack_watcher(mgr, dispatcher, stack_writer) => {
            format!("stack watcher exited: {:?}", r)
        }
        r = run_workspace_watcher(mgr, dispatcher, ws_writer) => {
            format!("workspace watcher exited: {:?}", r)
        }
        r = run_update_watcher(mgr, dispatcher, update_writer) => {
            format!("update watcher exited: {:?}", r)
        }
        r = run_program_watcher(mgr, dispatcher, program_writer) => {
            format!("program watcher exited: {:?}", r)
        }
        r = run_pool_evictor(mgr) => {
            format!("pool evictor exited: {:?}", r)
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("received shutdown signal");
            dispatcher.shutdown_all().await;
            return Ok(());
        }
    };

    tracing::error!(%exit_reason, "controller exited unexpectedly, shutting down for restart");
    dispatcher.shutdown_all().await;
    Err(crate::errors::RunError::ControllerExited(exit_reason))
}

async fn run_stack_watcher(
    mgr: &'static Manager,
    dispatcher: &'static Dispatcher,
    writer: reflector::store::Writer<Stack>,
) -> Result<(), crate::errors::RunError> {
    let stacks: Api<Stack> = Api::all(mgr.client.clone());
    let watcher_stream = watcher::watcher(stacks, WatcherConfig::default());
    let mut stream = reflector::reflector(writer, watcher_stream).boxed();

    while let Some(event) = stream.next().await {
        match event {
            Ok(watcher::Event::Apply(stack) | watcher::Event::InitApply(stack)) => {
                if let Some(ns) = stack.namespace() {
                    let key = NameKey::new(&ns, &stack.name_any());
                    dispatcher
                        .dispatch(
                            key,
                            StackMessage::Reconcile {
                                trigger: ReconcileTrigger::StackChanged,
                            },
                        )
                        .await;
                }
            }
            Ok(watcher::Event::Delete(stack)) => {
                if let Some(ns) = stack.namespace() {
                    let key = NameKey::new(&ns, &stack.name_any());
                    dispatcher
                        .dispatch(
                            key,
                            StackMessage::Reconcile {
                                trigger: ReconcileTrigger::StackChanged,
                            },
                        )
                        .await;
                }
            }
            Ok(watcher::Event::Init) | Ok(watcher::Event::InitDone) => {}
            Err(e) => {
                tracing::warn!(error = %e, "stack watcher error");
            }
        }
    }

    // Stream ended — this shouldn't happen normally.
    // The outer tokio::select! will log and shut down.
    tracing::warn!("stack watcher stream ended unexpectedly");
    Ok(())
}

async fn run_workspace_watcher(
    mgr: &'static Manager,
    dispatcher: &'static Dispatcher,
    writer: reflector::store::Writer<Workspace>,
) -> Result<(), crate::errors::RunError> {
    let workspaces: Api<Workspace> = Api::all(mgr.client.clone());
    let watcher_stream = watcher::watcher(workspaces, WatcherConfig::default());
    let mut stream = reflector::reflector(writer, watcher_stream).boxed();

    while let Some(event) = stream.next().await {
        match event {
            Ok(watcher::Event::Apply(ws) | watcher::Event::InitApply(ws)) => {
                if let Some(ns) = ws.namespace() {
                    if let Some(owner) = ws.owner_references().iter().find(|o| o.kind == "Stack") {
                        let key = NameKey::new(&ns, &owner.name);
                        dispatcher
                            .dispatch(
                                key,
                                StackMessage::Reconcile {
                                    trigger: ReconcileTrigger::WorkspaceChanged,
                                },
                            )
                            .await;
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "workspace watcher error");
            }
        }
    }

    Ok(())
}

async fn run_update_watcher(
    mgr: &'static Manager,
    dispatcher: &'static Dispatcher,
    writer: reflector::store::Writer<Update>,
) -> Result<(), crate::errors::RunError> {
    use crate::api::conditions::{UPDATE_COMPLETE, UPDATE_PROGRESSING};
    use compact_str::CompactString;

    let updates_api: Api<Update> = Api::all(mgr.client.clone());
    let watcher_stream = watcher::watcher(updates_api, WatcherConfig::default());
    let mut stream = reflector::reflector(writer, watcher_stream).boxed();

    while let Some(event) = stream.next().await {
        match event {
            Ok(watcher::Event::Apply(update) | watcher::Event::InitApply(update)) => {
                let ns = match update.namespace() {
                    Some(ns) => ns,
                    None => continue,
                };
                let name = update.name_any();

                // Check if this update needs execution (no Progressing or Complete condition)
                let needs_execution = {
                    let status = update.status.as_ref();
                    let has_progressing = status
                        .map(|s| {
                            s.conditions
                                .iter()
                                .any(|c| c.type_ == UPDATE_PROGRESSING && c.status == "True")
                        })
                        .unwrap_or(false);
                    let has_complete = status
                        .map(|s| {
                            s.conditions
                                .iter()
                                .any(|c| c.type_ == UPDATE_COMPLETE && c.status == "True")
                        })
                        .unwrap_or(false);
                    !has_progressing && !has_complete
                };

                if needs_execution {
                    // Check if already being processed (using Manager's persistent set)
                    let key_str = CompactString::new(format!("{}/{}", ns, name));
                    {
                        let mut set = mgr.update_in_progress.lock();
                        if set.contains(&key_str) {
                            continue;
                        }
                        set.insert(key_str.clone());
                    }

                    let workspace_name = match update.spec.workspace_name.as_deref() {
                        Some(ws) => ws.to_owned(),
                        None => {
                            tracing::error!(update = %name, "update has no workspaceName");
                            mgr.update_in_progress.lock().remove(&key_str);
                            continue;
                        }
                    };

                    let update_clone = update.clone();
                    let ns_clone = ns.clone();
                    let name_clone = name.clone();
                    let key_str_clone = key_str.clone();

                    // Spawn the update execution as a background task
                    tokio::spawn(async move {
                        execute_update(
                            mgr,
                            dispatcher,
                            &ns_clone,
                            &name_clone,
                            &workspace_name,
                            &update_clone,
                        )
                        .await;
                        mgr.update_in_progress.lock().remove(&key_str_clone);
                    });
                } else {
                    // Update already has status — notify Stack actor
                    if let Some(owner) =
                        update.owner_references().iter().find(|o| o.kind == "Stack")
                    {
                        let key = NameKey::new(&ns, &owner.name);
                        dispatcher
                            .dispatch(
                                key,
                                StackMessage::Reconcile {
                                    trigger: ReconcileTrigger::UpdateCompleted,
                                },
                            )
                            .await;
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "update watcher error");
            }
        }
    }

    Ok(())
}

/// Execute an Update by connecting to the workspace agent via gRPC.
async fn execute_update(
    mgr: &'static Manager,
    dispatcher: &'static Dispatcher,
    ns: &str,
    update_name: &str,
    workspace_name: &str,
    update: &Update,
) {
    use crate::api::conditions::{
        FIELD_MANAGER, UPDATE_COMPLETE, UPDATE_FAILED, UPDATE_PROGRESSING,
    };
    use crate::api::update::UpdateType;
    use crate::operator::controllers::update::{build_output_secret, stream_update};
    use kube::api::{Patch, PatchParams};

    let update_api: Api<Update> = Api::namespaced(mgr.client.clone(), ns);
    let now = chrono::Utc::now().to_rfc3339();

    // Step 1: Set Progressing=True
    let progressing_patch = serde_json::json!({
        "apiVersion": "auto.pulumi.com/v1alpha1",
        "kind": "Update",
        "status": {
            "startTime": &now,
            "conditions": [{
                "type": UPDATE_PROGRESSING,
                "status": "True",
                "reason": "UpdateStarted",
                "message": "Connecting to workspace agent",
                "lastTransitionTime": &now,
            }]
        }
    });
    if let Err(e) = update_api
        .patch_status(
            update_name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(&progressing_patch),
        )
        .await
    {
        tracing::error!(update = %update_name, error = %e, "failed to set progressing status");
        return;
    }
    tracing::info!(update = %update_name, "update execution started");

    // Step 2: Determine workspace address (use consistent helper)
    let workspace_addr =
        crate::operator::controllers::workspace::get_workspace_address(workspace_name, ns);

    // Step 3: Read Stack config and operation timeout (single fetch)
    let (stack_config, op_timeout) = {
        let stack_name = update
            .owner_references()
            .iter()
            .find(|o| o.kind == "Stack")
            .map(|o| o.name.clone());
        match stack_name {
            Some(ref name) => {
                let stacks: Api<crate::api::stack::Stack> = Api::namespaced(mgr.client.clone(), ns);
                match stacks.get(name).await {
                    Ok(s) => (
                        s.spec.config.clone(),
                        std::time::Duration::from_secs(
                            s.spec.operation_timeout_seconds.max(60) as u64
                        ),
                    ),
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to read Stack; proceeding with defaults");
                        (None, std::time::Duration::from_secs(3600))
                    }
                }
            }
            None => (None, std::time::Duration::from_secs(3600)),
        }
    };

    // Step 4: Stream the update via gRPC (with operation timeout)

    let result = match tokio::time::timeout(
        op_timeout,
        stream_update(mgr, ns, &workspace_addr, update, stack_config.as_ref()),
    )
    .await
    {
        Ok(r) => r,
        Err(_) => {
            tracing::error!(
                update = %update_name,
                timeout_secs = op_timeout.as_secs(),
                "stream_update timed out"
            );
            Err(crate::errors::OperatorError::Transient(
                crate::errors::TransientError::OperationTimeout,
            ))
        }
    };
    let end_time = chrono::Utc::now().to_rfc3339();

    match result {
        Ok(stream_result) => {
            if let Some(ref err_msg) = stream_result.error {
                // Update failed
                tracing::error!(update = %update_name, error = %err_msg, "update failed");
                let failed_patch = serde_json::json!({
                    "apiVersion": "auto.pulumi.com/v1alpha1",
                    "kind": "Update",
                    "status": {
                        "startTime": &now,
                        "endTime": &end_time,
                        "message": err_msg,
                        "conditions": [
                            {
                                "type": UPDATE_PROGRESSING,
                                "status": "False",
                                "reason": "UpdateComplete",
                                "lastTransitionTime": &end_time,
                            },
                            {
                                "type": UPDATE_COMPLETE,
                                "status": "True",
                                "reason": "UpdateFailed",
                                "message": err_msg,
                                "lastTransitionTime": &end_time,
                            },
                            {
                                "type": UPDATE_FAILED,
                                "status": "True",
                                "reason": "UpdateFailed",
                                "message": err_msg,
                                "lastTransitionTime": &end_time,
                            }
                        ]
                    }
                });
                if let Err(e) = update_api
                    .patch_status(
                        update_name,
                        &PatchParams::apply(FIELD_MANAGER).force(),
                        &Patch::Apply(&failed_patch),
                    )
                    .await
                {
                    tracing::error!(update = %update_name, error = %e, "failed to patch failed status");
                }
            } else {
                // Update succeeded
                tracing::info!(
                    update = %update_name,
                    permalink = ?stream_result.permalink,
                    "update succeeded"
                );

                // Create output Secret for Up operations
                if matches!(update.spec.update_type.as_ref(), Some(UpdateType::Up))
                    && !stream_result.outputs.is_empty()
                {
                    let owner_ref =
                        k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference {
                            api_version: "auto.pulumi.com/v1alpha1".to_owned(),
                            kind: "Update".to_owned(),
                            name: update_name.to_owned(),
                            uid: update.metadata.uid.clone().unwrap_or_default(),
                            controller: Some(true),
                            block_owner_deletion: Some(false),
                        };
                    let secret = build_output_secret(
                        update_name,
                        ns,
                        &stream_result.outputs,
                        &[], // No specific secret keys
                        owner_ref,
                    );
                    let secrets_api: Api<k8s_openapi::api::core::v1::Secret> =
                        Api::namespaced(mgr.client.clone(), ns);
                    if let Err(e) = secrets_api
                        .create(&kube::api::PostParams::default(), &secret)
                        .await
                    {
                        tracing::warn!(
                            update = %update_name,
                            error = %e,
                            "failed to create output secret (may already exist)"
                        );
                    }
                }

                // Serialize outputs for status
                let outputs_json = if !stream_result.outputs.is_empty() {
                    let map: std::collections::BTreeMap<String, String> = stream_result
                        .outputs
                        .iter()
                        .map(|(k, v)| (k.clone(), String::from_utf8_lossy(&v.value).into_owned()))
                        .collect();
                    serde_json::to_string(&map).ok()
                } else {
                    None
                };

                let success_patch = serde_json::json!({
                    "apiVersion": "auto.pulumi.com/v1alpha1",
                    "kind": "Update",
                    "status": {
                        "startTime": &now,
                        "endTime": &end_time,
                        "permalink": stream_result.permalink,
                        "outputs": outputs_json,
                        "conditions": [
                            {
                                "type": UPDATE_PROGRESSING,
                                "status": "False",
                                "reason": "UpdateComplete",
                                "lastTransitionTime": &end_time,
                            },
                            {
                                "type": UPDATE_COMPLETE,
                                "status": "True",
                                "reason": "UpdateSucceeded",
                                "message": "Update completed successfully",
                                "lastTransitionTime": &end_time,
                            }
                        ]
                    }
                });
                if let Err(e) = update_api
                    .patch_status(
                        update_name,
                        &PatchParams::apply(FIELD_MANAGER).force(),
                        &Patch::Apply(&success_patch),
                    )
                    .await
                {
                    tracing::error!(update = %update_name, error = %e, "failed to patch success status");
                }
            }
        }
        Err(e) => {
            tracing::error!(update = %update_name, error = %e, "stream_update error");
            let error_msg = format!("{}", e);
            let error_patch = serde_json::json!({
                "apiVersion": "auto.pulumi.com/v1alpha1",
                "kind": "Update",
                "status": {
                    "startTime": &now,
                    "endTime": &end_time,
                    "message": &error_msg,
                    "conditions": [
                        {
                            "type": UPDATE_PROGRESSING,
                            "status": "False",
                            "reason": "UpdateComplete",
                            "lastTransitionTime": &end_time,
                        },
                        {
                            "type": UPDATE_COMPLETE,
                            "status": "True",
                            "reason": "UpdateFailed",
                            "message": &error_msg,
                            "lastTransitionTime": &end_time,
                        },
                        {
                            "type": UPDATE_FAILED,
                            "status": "True",
                            "reason": "UpdateFailed",
                            "message": &error_msg,
                            "lastTransitionTime": &end_time,
                        }
                    ]
                }
            });
            if let Err(e2) = update_api
                .patch_status(
                    update_name,
                    &PatchParams::apply(FIELD_MANAGER).force(),
                    &Patch::Apply(&error_patch),
                )
                .await
            {
                tracing::error!(update = %update_name, error = %e2, "failed to patch error status");
            }
        }
    }

    // Step 4: Notify Stack actor about the completed update
    if let Some(owner) = update.owner_references().iter().find(|o| o.kind == "Stack") {
        let key = NameKey::new(ns, &owner.name);
        dispatcher
            .dispatch(
                key,
                StackMessage::Reconcile {
                    trigger: ReconcileTrigger::UpdateCompleted,
                },
            )
            .await;
    }
}

async fn run_program_watcher(
    mgr: &'static Manager,
    dispatcher: &'static Dispatcher,
    writer: reflector::store::Writer<Program>,
) -> Result<(), crate::errors::RunError> {
    use crate::api::conditions::{FIELD_MANAGER, PROGRAM_FINALIZER};
    use crate::operator::controllers::program::{
        build_artifact, reconcile_program, ProgramReconcileAction, FILE_SERVER_PORT,
    };
    use kube::api::{Patch, PatchParams};

    let programs: Api<Program> = Api::all(mgr.client.clone());
    let watcher_stream = watcher::watcher(programs, WatcherConfig::default());
    let mut stream = reflector::reflector(writer, watcher_stream).boxed();

    // Determine the file server address (reachable from workspace pods)
    let server_addr = file_server_address(mgr, FILE_SERVER_PORT);

    while let Some(event) = stream.next().await {
        match event {
            Ok(watcher::Event::Apply(prog) | watcher::Event::InitApply(prog)) => {
                let ns = match prog.namespace() {
                    Some(ns) => ns,
                    None => continue,
                };
                let name = prog.name_any();

                match reconcile_program(mgr, &prog).await {
                    Ok(ProgramReconcileAction::AddFinalizer) => {
                        let patch = serde_json::json!({
                            "apiVersion": "pulumi.com/v1",
                            "kind": "Program",
                            "metadata": {
                                "name": &name,
                                "finalizers": [PROGRAM_FINALIZER]
                            }
                        });
                        let api: Api<Program> = Api::namespaced(mgr.client.clone(), &ns);
                        if let Err(e) = api
                            .patch(
                                &name,
                                &PatchParams::apply(FIELD_MANAGER),
                                &Patch::Apply(&patch),
                            )
                            .await
                        {
                            tracing::error!(program = %name, error = %e, "failed to add program finalizer");
                        }
                    }
                    Ok(ProgramReconcileAction::RemoveFinalizer) => {
                        let mut finalizers: Vec<String> = prog.finalizers().to_vec();
                        finalizers.retain(|f| f != PROGRAM_FINALIZER);
                        let patch = serde_json::json!({
                            "metadata": { "finalizers": finalizers }
                        });
                        let api: Api<Program> = Api::namespaced(mgr.client.clone(), &ns);
                        if let Err(e) = api
                            .patch(&name, &PatchParams::default(), &Patch::Merge(&patch))
                            .await
                        {
                            tracing::error!(program = %name, error = %e, "failed to remove program finalizer");
                        }
                    }
                    Ok(ProgramReconcileAction::EnsureServing) => {
                        let generation = prog.metadata.generation.unwrap_or(0);
                        match build_artifact(&prog.spec, &ns, &name, generation, &server_addr) {
                            Ok((artifact, data)) => {
                                mgr.program_file_server.store_artifact(&artifact.path, data);
                                tracing::info!(
                                    program = %name,
                                    url = %artifact.url,
                                    "program artifact built and stored"
                                );

                                // Update Program status with artifact
                                let api: Api<Program> = Api::namespaced(mgr.client.clone(), &ns);
                                let status_patch = serde_json::json!({
                                    "apiVersion": "pulumi.com/v1",
                                    "kind": "Program",
                                    "status": {
                                        "observedGeneration": generation,
                                        "artifact": artifact,
                                    }
                                });
                                if let Err(e) = api
                                    .patch_status(
                                        &name,
                                        &PatchParams::apply(FIELD_MANAGER).force(),
                                        &Patch::Apply(&status_patch),
                                    )
                                    .await
                                {
                                    tracing::error!(
                                        program = %name,
                                        error = %e,
                                        "failed to update program status"
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::error!(
                                    program = %name,
                                    error = %e,
                                    "failed to build program artifact"
                                );
                            }
                        }

                        // Trigger reconcile for all stacks that reference this program
                        trigger_referencing_stacks(mgr, dispatcher, &ns, &name).await;
                    }
                    Ok(ProgramReconcileAction::BlockDeletion { referencing_count }) => {
                        tracing::info!(
                            program = %name,
                            referencing_count,
                            "program deletion blocked, still referenced by stacks"
                        );
                    }
                    Err(e) => {
                        tracing::error!(program = %name, error = %e, "program reconcile error");
                    }
                }
            }
            Ok(watcher::Event::Delete(prog)) => {
                // Clean up artifact from file server
                if let Some(ns) = prog.namespace() {
                    let name = prog.name_any();
                    let gen = prog.metadata.generation.unwrap_or(0);
                    let path = format!("programs/{}/{}/{}.tar.gz", ns, name, gen);
                    mgr.program_file_server.remove_artifact(&path);
                    tracing::debug!(program = %name, "removed program artifact");
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "program watcher error");
            }
        }
    }

    Ok(())
}

/// Trigger reconciles for all Stacks that reference the given Program.
async fn trigger_referencing_stacks(
    mgr: &'static Manager,
    dispatcher: &'static Dispatcher,
    ns: &str,
    program_name: &str,
) {
    for stack in mgr.stores.stacks.state().iter() {
        if stack.namespace().as_deref() == Some(ns)
            && stack
                .spec
                .program_ref
                .as_ref()
                .is_some_and(|pr| pr.name == program_name)
        {
            let key = NameKey::new(ns, &stack.name_any());
            dispatcher
                .dispatch(
                    key,
                    StackMessage::Reconcile {
                        trigger: ReconcileTrigger::StackChanged,
                    },
                )
                .await;
        }
    }
}

/// Compute the file server address reachable from workspace pods.
/// Uses the operator's service DNS name within the cluster.
fn file_server_address(_mgr: &Manager, port: u16) -> String {
    // Use the OPERATOR_SERVICE_NAME env var if set (from Helm chart),
    // otherwise fall back to a reasonable default.
    let svc_name =
        std::env::var("OPERATOR_SERVICE_NAME").unwrap_or_else(|_| "pulumi-operator".to_owned());
    let svc_ns = std::env::var("OPERATOR_NAMESPACE").unwrap_or_else(|_| {
        // Try to read from the downward API
        std::fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/namespace")
            .unwrap_or_else(|_| "pulumi-system".to_owned())
            .trim()
            .to_owned()
    });
    format!("{}.{}.svc.cluster.local:{}", svc_name, svc_ns, port)
}

/// Background task to evict idle connections every 5 minutes (matches Go: 5-minute prune interval).
async fn run_pool_evictor(mgr: &'static Manager) -> Result<(), crate::errors::RunError> {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
    loop {
        interval.tick().await;
        mgr.pool.evict_idle();
        mgr.metrics.set_pool_size(mgr.pool.len() as u64);
    }
}
