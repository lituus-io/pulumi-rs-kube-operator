use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::time::{Duration, Instant};

use compact_str::CompactString;
use kube::{Api, ResourceExt};
use tokio::sync::mpsc;

use crate::api::conditions::STACK_FINALIZER;
use crate::api::stack::{Stack, WorkspaceReclaimPolicy};
use crate::api::update::Update;
use crate::api::workspace::Workspace;
use crate::core::lending::Lend;
use crate::errors::OperatorError;
use crate::operator::lock::{LockAction, LockState};
use crate::operator::manager::Manager;
use crate::core::recovery::recovery_action;

use super::messages::{NameKey, PrioritizedMessage, Priority, ReconcileTrigger, StackMessage};

/// Per-stack actor. Borrows &'static Manager -- no Arc.
pub struct Actor {
    pub mgr: &'static Manager,
    pub key: NameKey,
    pub state: ActorState,
    pub mailbox: mpsc::Receiver<PrioritizedMessage>,
    /// Sender for self-requeue (delayed retries). Cloned from the dispatcher's channel.
    requeue_tx: mpsc::Sender<PrioritizedMessage>,
}

/// Private per-actor state -- no Mutex needed, only one task accesses it.
pub struct ActorState {
    pub failure_count: u32,
    pub lock_state: LockState,
    pub last_commit: Option<CompactString>,
    pub last_generation: i64,
    /// Debounce: earliest Instant at which a reconcile is allowed.
    /// Set when a retry is scheduled (e.g. after failure cooldown).
    /// Prevents watcher events from bypassing scheduled backoff.
    pub next_reconcile_at: Option<Instant>,
}

impl Default for ActorState {
    fn default() -> Self {
        Self {
            failure_count: 0,
            lock_state: LockState::new(Duration::from_secs(900)),
            last_commit: None,
            last_generation: 0,
            next_reconcile_at: None,
        }
    }
}

/// Wrapper for BinaryHeap ordering (higher priority = lower enum value = first out).
struct HeapEntry(PrioritizedMessage);

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.0.priority == other.0.priority
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse: lower enum value = higher priority = comes first
        other.0.priority.cmp(&self.0.priority)
    }
}

impl Actor {
    pub fn new(
        mgr: &'static Manager,
        key: NameKey,
        mailbox: mpsc::Receiver<PrioritizedMessage>,
        requeue_tx: mpsc::Sender<PrioritizedMessage>,
    ) -> Self {
        Self {
            mgr,
            key,
            state: ActorState::default(),
            mailbox,
            requeue_tx,
        }
    }

    /// Main actor loop. Drains mailbox into priority queue, processes highest-priority first.
    /// Deletion (0) > LockRecovery (1) > FailureRetry (2) > Normal (3).
    pub async fn run(mut self) {
        self.mgr.metrics.inc_active_actors();
        tracing::info!(key = %self.key, "actor started");

        let mut heap = BinaryHeap::<HeapEntry>::new();

        while let Some(first) = self.mailbox.recv().await {
            // Drain all pending messages into the priority heap
            heap.push(HeapEntry(first));
            while let Ok(msg) = self.mailbox.try_recv() {
                heap.push(HeapEntry(msg));
            }

            // Process in priority order
            while let Some(HeapEntry(msg)) = heap.pop() {
                match msg.inner {
                    StackMessage::Reconcile { trigger } => {
                        self.handle_reconcile(trigger).await;
                    }
                    StackMessage::Shutdown => {
                        tracing::info!(key = %self.key, "actor shutting down");
                        self.mgr.metrics.dec_active_actors();
                        tracing::info!(key = %self.key, "actor stopped");
                        return;
                    }
                }
            }
        }

        self.mgr.metrics.dec_active_actors();
        tracing::info!(key = %self.key, "actor stopped");
    }

    /// Schedule a delayed self-requeue. Spawns a background task that sleeps then
    /// sends a reconcile message to this actor's mailbox.
    /// Also sets the debounce window so watcher events are ignored until the retry fires.
    fn schedule_requeue(&mut self, delay: Duration, priority: Priority, trigger: ReconcileTrigger) {
        self.state.next_reconcile_at = Some(Instant::now() + delay);
        let tx = self.requeue_tx.clone();
        let key = self.key.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let msg = PrioritizedMessage {
                priority,
                inner: StackMessage::Reconcile { trigger },
            };
            if tx.send(msg).await.is_err() {
                tracing::debug!(key = %key, "requeue dropped (actor gone)");
            }
        });
    }

    async fn handle_reconcile(&mut self, trigger: ReconcileTrigger) {
        // Debounce: skip reconciles that arrive before the scheduled retry time.
        // This prevents watcher events from bypassing cooldown/backoff schedules.
        // Only scheduled retries (which clear the debounce) can proceed during cooldown.
        if let Some(next_at) = self.state.next_reconcile_at {
            if Instant::now() < next_at && trigger != ReconcileTrigger::Retry && trigger != ReconcileTrigger::LockRetry {
                tracing::debug!(key = %self.key, ?trigger, "reconcile debounced, waiting for scheduled retry");
                return;
            }
            // Clear the debounce — we're proceeding
            self.state.next_reconcile_at = None;
        }

        self.mgr.metrics.inc_reconciles();
        tracing::debug!(key = %self.key, ?trigger, "reconcile triggered");

        let stack_api: Api<Stack> = Api::namespaced(
            self.mgr.client.clone(),
            self.key.ns.as_str(),
        );

        let stack = match stack_api.get(self.key.name.as_str()).await {
            Ok(s) => s,
            Err(kube::Error::Api(err)) if err.code == 404 => {
                tracing::info!(key = %self.key, "stack deleted, cleaning up orphaned updates");
                self.cleanup_orphaned_updates().await;
                return;
            }
            Err(e) => {
                tracing::error!(key = %self.key, error = %e, "failed to get stack");
                self.mgr.metrics.inc_reconcile_errors();
                self.schedule_requeue(
                    Duration::from_secs(5),
                    Priority::FailureRetry,
                    ReconcileTrigger::Retry,
                );
                return;
            }
        };

        // Sync lock timeout from Stack spec (user-configurable, default 900s)
        let lock_timeout_secs = stack.spec.lock_timeout_seconds.max(60) as u64;
        self.state.lock_state.set_timeout(Duration::from_secs(lock_timeout_secs));

        // Handle LockRetry: force-cancel before proceeding with normal reconciliation
        if trigger == ReconcileTrigger::LockRetry {
            self.handle_force_unlock(&stack).await;
        }

        let result = crate::operator::reconcile::pipeline::run_pipeline(
            self.mgr,
            &mut self.state,
            &self.key,
            &stack,
        )
        .await;

        match result {
            Ok(ref action) => {
                self.handle_action(action, &stack).await;
                tracing::debug!(key = %self.key, ?action, "reconcile completed");
            }
            Err(ref e) => {
                self.mgr.metrics.inc_reconcile_errors();
                self.handle_error(e, &stack).await;
            }
        }
    }

    async fn handle_action(
        &mut self,
        action: &crate::operator::reconcile::pipeline::ReconcileAction,
        stack: &Stack,
    ) {
        use crate::api::conditions::*;
        use crate::operator::reconcile::pipeline::ReconcileAction as RA;
        use kube::api::{Patch, PatchParams};

        let stack_api: Api<Stack> = Api::namespaced(
            self.mgr.client.clone(),
            self.key.ns.as_str(),
        );
        let name = self.key.name.as_str();

        match action {
            RA::AddFinalizer => {
                tracing::debug!(key = %self.key, "adding finalizer");
                let patch = serde_json::json!({
                    "apiVersion": "pulumi.com/v1",
                    "kind": "Stack",
                    "metadata": {
                        "name": name,
                        "finalizers": [STACK_FINALIZER]
                    }
                });
                if let Err(e) = stack_api
                    .patch(name, &PatchParams::apply(STACK_FINALIZER_FM), &Patch::Apply(&patch))
                    .await
                {
                    tracing::error!(key = %self.key, error = %e, "failed to add finalizer");
                }
            }
            RA::RemoveFinalizer => {
                tracing::debug!(key = %self.key, "removing finalizer");
                self.remove_stack_finalizer(&stack_api, name, stack).await;
            }
            RA::RemoveFinalizerAfterDestroy => {
                tracing::info!(key = %self.key, "destroy completed, removing finalizer");
                self.mgr.events.record(
                    &event_ref_helper::stack_ref(stack),
                    &crate::operator::events::StackEvent::DestroySucceeded,
                    stack,
                    &self.mgr.metrics,
                );
                // Delete workspace before removing finalizer (cleanup)
                if stack.spec.workspace_reclaim_policy == WorkspaceReclaimPolicy::Delete {
                    self.delete_workspace(name).await;
                }
                self.remove_stack_finalizer(&stack_api, name, stack).await;
            }
            RA::UpdateCreated { name: update_name } => {
                tracing::info!(key = %self.key, update = %update_name, "update created");
                self.mgr.events.record(
                    &event_ref_helper::stack_ref(stack),
                    &crate::operator::events::StackEvent::UpdateCreated { update_name },
                    stack,
                    &self.mgr.metrics,
                );
                self.state.lock_state.on_success(); // Clear lock state on new update
                self.state.failure_count = 0;
                let generation = stack.metadata.generation.unwrap_or(0);
                let now = chrono::Utc::now().to_rfc3339();
                use crate::operator::status::{stack_patch, condition};
                let patch = stack_patch(serde_json::json!({
                    "currentUpdate": {
                        "generation": generation,
                        "name": update_name,
                    },
                    "observedGeneration": generation,
                    "conditions": [
                        condition(RECONCILING, "True", RECONCILING_PROCESSING,
                            format!("Update {} in progress", update_name), &now, generation),
                        condition(READY, "False", NOT_READY_IN_PROGRESS,
                            "Update in progress", &now, generation),
                    ]
                }));
                if let Err(e) = stack_api
                    .patch_status(name, &PatchParams::apply(FIELD_MANAGER).force(), &Patch::Apply(&patch))
                    .await
                {
                    tracing::error!(key = %self.key, error = %e, "failed to update status for UpdateCreated");
                }
            }
            RA::UpdateSucceeded { name: update_name, permalink, outputs } => {
                tracing::info!(
                    key = %self.key, update = %update_name,
                    permalink = permalink.as_deref().unwrap_or(""),
                    "update succeeded"
                );
                self.mgr.events.record(
                    &event_ref_helper::stack_ref(stack),
                    &crate::operator::events::StackEvent::UpdateSucceeded {
                        update_name,
                        permalink: permalink.as_deref(),
                    },
                    stack,
                    &self.mgr.metrics,
                );
                self.state.failure_count = 0;
                self.state.lock_state.on_success();
                let generation = stack.metadata.generation.unwrap_or(0);
                let now = chrono::Utc::now().to_rfc3339();
                let outputs_value: Option<serde_json::Value> = outputs
                    .as_ref()
                    .and_then(|o| serde_json::from_str(o).ok());
                use crate::operator::status::{stack_patch, condition};
                let mut status = serde_json::json!({
                    "currentUpdate": null,
                    "lastUpdate": {
                        "generation": generation,
                        "name": update_name,
                        "type": "up",
                        "state": "succeeded",
                        "permalink": permalink,
                        "lastSuccessfulCommit": self.state.last_commit.as_deref(),
                        "lastResyncTime": now,
                        "failures": 0,
                    },
                    "observedGeneration": generation,
                    "conditions": [
                        condition(READY, "True", READY_COMPLETED,
                            "Stack update succeeded", &now, generation),
                        condition(RECONCILING, "False", READY_COMPLETED,
                            "", &now, generation),
                    ]
                });
                if let Some(outputs_val) = outputs_value {
                    status["outputs"] = outputs_val;
                }
                let patch = stack_patch(status);
                if let Err(e) = stack_api
                    .patch_status(name, &PatchParams::apply(FIELD_MANAGER).force(), &Patch::Apply(&patch))
                    .await
                {
                    tracing::error!(key = %self.key, error = %e, "failed to update status for UpdateSucceeded");
                }

                // Remove update finalizer — the update's job is done
                self.remove_update_finalizer(self.key.ns.as_str(), update_name).await;

                // Delete workspace if reclaim policy is Delete (ephemeral workspaces)
                if stack.spec.workspace_reclaim_policy == WorkspaceReclaimPolicy::Delete {
                    self.delete_workspace(name).await;
                }
            }
            RA::UpdateFailed { name: update_name, message } => {
                let is_lock = is_lock_error(message);
                tracing::warn!(
                    key = %self.key, update = %update_name,
                    message, is_lock,
                    "update failed"
                );
                if is_lock {
                    self.mgr.events.record(
                        &event_ref_helper::stack_ref(stack),
                        &crate::operator::events::StackEvent::LockConflict { update_name },
                        stack,
                        &self.mgr.metrics,
                    );
                } else {
                    self.mgr.events.record(
                        &event_ref_helper::stack_ref(stack),
                        &crate::operator::events::StackEvent::UpdateFailed { update_name, message },
                        stack,
                        &self.mgr.metrics,
                    );
                }
                self.state.failure_count += 1;
                let generation = stack.metadata.generation.unwrap_or(0);
                let now = chrono::Utc::now().to_rfc3339();
                let failures = self.state.failure_count as i64;
                use crate::operator::status::{stack_patch, condition};
                let patch = stack_patch(serde_json::json!({
                    "currentUpdate": null,
                    "lastUpdate": {
                        "generation": generation,
                        "name": update_name,
                        "type": "up",
                        "state": "failed",
                        "message": message,
                        "lastAttemptedCommit": self.state.last_commit.as_deref(),
                        "lastResyncTime": now,
                        "failures": failures,
                    },
                    "observedGeneration": generation,
                    "conditions": [
                        condition(READY, "False", NOT_READY_IN_PROGRESS,
                            format!("Update failed: {}", message), &now, generation),
                        condition(RECONCILING, "True", RECONCILING_RETRY,
                            format!("Retrying after failure (attempt {})", failures), &now, generation),
                    ]
                }));
                if let Err(e) = stack_api
                    .patch_status(name, &PatchParams::apply(FIELD_MANAGER).force(), &Patch::Apply(&patch))
                    .await
                {
                    tracing::error!(key = %self.key, error = %e, "failed to update status for UpdateFailed");
                }

                // Remove update finalizer — the update's job is done
                self.remove_update_finalizer(self.key.ns.as_str(), update_name).await;

                // Handle lock conflicts: track in LockState and schedule recovery
                if is_lock {
                    self.mgr.metrics.inc_lock_conflicts();
                    match self.state.lock_state.on_conflict() {
                        LockAction::ForceUnlock => {
                            tracing::warn!(
                                key = %self.key,
                                "lock timeout exceeded, scheduling force-unlock via CancelUpdate"
                            );
                            self.mgr.metrics.inc_force_unlocks();
                            // Schedule immediate lock recovery reconcile
                            self.schedule_requeue(
                                Duration::from_secs(2),
                                Priority::LockRecovery,
                                ReconcileTrigger::LockRetry,
                            );
                        }
                        LockAction::RetryAfter(d) => {
                            tracing::info!(
                                key = %self.key,
                                delay_secs = d.as_secs(),
                                "lock conflict, scheduling retry after backoff"
                            );
                            self.schedule_requeue(
                                d,
                                Priority::FailureRetry,
                                ReconcileTrigger::Retry,
                            );
                        }
                        LockAction::Clear => {
                            self.state.lock_state.on_success();
                        }
                    }
                } else {
                    // Non-lock failure: schedule retry after cooldown
                    // Cooldown is exponential: 10s * 3^failures, capped by spec
                    let cooldown = crate::operator::reconcile::sync::cooldown(
                        failures,
                        stack,
                    );
                    tracing::info!(
                        key = %self.key,
                        cooldown_secs = cooldown.as_secs(),
                        failures,
                        "scheduling retry after cooldown"
                    );
                    self.schedule_requeue(
                        cooldown,
                        Priority::FailureRetry,
                        ReconcileTrigger::Retry,
                    );
                }
            }
            RA::DestroyStarted { name: update_name } => {
                tracing::info!(key = %self.key, update = %update_name, "destroy update created, waiting for completion");
                self.mgr.events.record(
                    &event_ref_helper::stack_ref(stack),
                    &crate::operator::events::StackEvent::DestroyStarted { update_name },
                    stack,
                    &self.mgr.metrics,
                );
                let generation = stack.metadata.generation.unwrap_or(0);
                let now = chrono::Utc::now().to_rfc3339();
                use crate::operator::status::{stack_patch, condition};
                let patch = stack_patch(serde_json::json!({
                    "currentUpdate": {
                        "generation": generation,
                        "name": update_name,
                    },
                    "observedGeneration": generation,
                    "conditions": [
                        condition(RECONCILING, "True", RECONCILING_PROCESSING,
                            format!("Destroy {} in progress", update_name), &now, generation),
                    ]
                }));
                if let Err(e) = stack_api
                    .patch_status(name, &PatchParams::apply(FIELD_MANAGER).force(), &Patch::Apply(&patch))
                    .await
                {
                    tracing::error!(key = %self.key, error = %e, "failed to update status for DestroyStarted");
                }
            }
            RA::DestroyFailed { name: update_name, failures } => {
                tracing::warn!(
                    key = %self.key, update = %update_name, failures,
                    "destroy update failed, will retry"
                );
                self.mgr.events.record(
                    &event_ref_helper::stack_ref(stack),
                    &crate::operator::events::StackEvent::DestroyFailed {
                        update_name,
                        attempt: *failures,
                    },
                    stack,
                    &self.mgr.metrics,
                );
                self.state.failure_count = (*failures).min(u32::MAX as i64) as u32;
                let generation = stack.metadata.generation.unwrap_or(0);
                let now = chrono::Utc::now().to_rfc3339();
                use crate::operator::status::{stack_patch, condition};
                let patch = stack_patch(serde_json::json!({
                    "currentUpdate": null,
                    "lastUpdate": {
                        "generation": generation,
                        "name": update_name,
                        "type": "destroy",
                        "state": "failed",
                        "lastResyncTime": now,
                        "failures": failures,
                    },
                    "observedGeneration": generation,
                    "conditions": [
                        condition(RECONCILING, "True", RECONCILING_RETRY,
                            format!("Destroy failed, retrying (attempt {})", failures), &now, generation),
                    ]
                }));
                if let Err(e) = stack_api
                    .patch_status(name, &PatchParams::apply(FIELD_MANAGER).force(), &Patch::Apply(&patch))
                    .await
                {
                    tracing::error!(key = %self.key, error = %e, "failed to update status for DestroyFailed");
                }
                // Schedule retry for destroy
                let cooldown = crate::operator::reconcile::sync::cooldown(*failures, stack);
                self.schedule_requeue(cooldown, Priority::FailureRetry, ReconcileTrigger::Retry);
            }
            RA::WaitForUpdate { name: update_name } => {
                tracing::debug!(key = %self.key, update = %update_name, "waiting for update");
                // Schedule a check-back in case the update watcher event is missed
                self.schedule_requeue(
                    Duration::from_secs(30),
                    Priority::Normal,
                    ReconcileTrigger::Retry,
                );
            }
            RA::WaitForWorkspace => {
                tracing::debug!(key = %self.key, "waiting for workspace");
                // Schedule retry to check workspace readiness
                self.schedule_requeue(
                    Duration::from_secs(5),
                    Priority::Normal,
                    ReconcileTrigger::Retry,
                );
            }
            RA::ProjectNotFound { project_id } => {
                tracing::warn!(
                    key = %self.key, project = %project_id,
                    "project not found, starting/continuing grace period"
                );
                self.mgr.events.record(
                    &event_ref_helper::stack_ref(stack),
                    &crate::operator::events::StackEvent::ProjectNotFound { project_id },
                    stack,
                    &self.mgr.metrics,
                );
                let generation = stack.metadata.generation.unwrap_or(0);
                let now = chrono::Utc::now().to_rfc3339();

                // Set pendingDeletionSince if not already set
                let pending_since = stack
                    .status
                    .as_ref()
                    .and_then(|s| s.pending_deletion_since.as_deref())
                    .unwrap_or(&now);

                let check_status = crate::operator::reconcile::project::build_check_status(
                    &crate::operator::reconcile::project::ProjectCheckResult::NotFound {
                        project_id: project_id.clone(),
                    },
                );
                use crate::operator::status::{stack_patch, condition};
                let patch = stack_patch(serde_json::json!({
                    "pendingDeletionSince": pending_since,
                    "lastProjectCheck": check_status,
                    "observedGeneration": generation,
                    "conditions": [
                        condition(PENDING_DELETION, "True", PENDING_DELETION_PROJECT,
                            format!("Project {} not found, grace period active", project_id), &now, generation),
                    ]
                }));
                if let Err(e) = stack_api
                    .patch_status(name, &PatchParams::apply(FIELD_MANAGER).force(), &Patch::Apply(&patch))
                    .await
                {
                    tracing::error!(key = %self.key, error = %e, "failed to update status for ProjectNotFound");
                }
                // Recheck in 5 minutes
                self.schedule_requeue(
                    Duration::from_secs(300),
                    Priority::Normal,
                    ReconcileTrigger::Retry,
                );
            }
            RA::ProjectReinstated => {
                tracing::info!(key = %self.key, "project reinstated, clearing pending deletion");
                let generation = stack.metadata.generation.unwrap_or(0);
                let now = chrono::Utc::now().to_rfc3339();

                let check_status = crate::operator::reconcile::project::build_check_status(
                    &crate::operator::reconcile::project::ProjectCheckResult::Active,
                );
                use crate::operator::status::{stack_patch, condition};
                let patch = stack_patch(serde_json::json!({
                    "pendingDeletionSince": null,
                    "lastProjectCheck": check_status,
                    "observedGeneration": generation,
                    "conditions": [
                        condition(PENDING_DELETION, "False", PENDING_DELETION_REINSTATED,
                            "Project found, pending deletion cleared", &now, generation),
                    ]
                }));
                if let Err(e) = stack_api
                    .patch_status(name, &PatchParams::apply(FIELD_MANAGER).force(), &Patch::Apply(&patch))
                    .await
                {
                    tracing::error!(key = %self.key, error = %e, "failed to update status for ProjectReinstated");
                }
                // Immediately requeue to continue normal reconciliation
                self.schedule_requeue(
                    Duration::from_secs(1),
                    Priority::Normal,
                    ReconcileTrigger::Retry,
                );
            }
            RA::ProjectTtlExpired => {
                tracing::warn!(key = %self.key, "project grace period expired, taking action");
                self.mgr.events.record(
                    &event_ref_helper::stack_ref(stack),
                    &crate::operator::events::StackEvent::ProjectTtlExpired,
                    stack,
                    &self.mgr.metrics,
                );
                let generation = stack.metadata.generation.unwrap_or(0);
                let now = chrono::Utc::now().to_rfc3339();

                let action = stack
                    .spec
                    .project_verification
                    .as_ref()
                    .map(|v| &v.on_grace_period_expired)
                    .cloned()
                    .unwrap_or_default();

                use crate::operator::status::{stack_patch, condition};
                let patch = stack_patch(serde_json::json!({
                    "observedGeneration": generation,
                    "conditions": [
                        condition(PENDING_DELETION, "True", PENDING_DELETION_TTL_EXPIRED,
                            "Grace period expired, executing cleanup", &now, generation),
                    ]
                }));
                if let Err(e) = stack_api
                    .patch_status(name, &PatchParams::apply(FIELD_MANAGER).force(), &Patch::Apply(&patch))
                    .await
                {
                    tracing::error!(key = %self.key, error = %e, "failed to update status for ProjectTtlExpired");
                }

                match action {
                    crate::api::stack::GracePeriodAction::DeleteKustomization => {
                        // Find and delete the Flux Kustomization that owns this Stack
                        self.delete_owning_kustomization(stack).await;
                    }
                    crate::api::stack::GracePeriodAction::RemoveFinalizer => {
                        // Delete workspace and remove finalizer directly
                        if stack.spec.workspace_reclaim_policy == WorkspaceReclaimPolicy::Delete {
                            self.delete_workspace(name).await;
                        }
                        self.remove_stack_finalizer(&stack_api, name, stack).await;
                    }
                }
            }
            RA::Synced | RA::Done => {
                // No action needed
            }
        }
    }

    /// Force-unlock a stale Pulumi backend lock by calling CancelUpdate on the workspace agent.
    /// Returns true if the lock was successfully cleared.
    async fn handle_force_unlock(&mut self, stack: &Stack) -> bool {
        let ns = self.key.ns.as_str();
        let workspace_name = self.key.name.as_str();

        // Resolve workspace gRPC address
        let workspace_addr = crate::operator::controllers::workspace::get_workspace_address(
            workspace_name,
            ns,
        );

        tracing::warn!(
            key = %self.key,
            addr = %workspace_addr,
            "attempting force-unlock via CancelUpdate"
        );

        // Try to get a connection and call cancel_update
        match self.mgr.pool.lend(&workspace_addr).await {
            Ok(guard) => {
                let channel = guard.channel();
                match crate::agent::cancel::cancel_update(channel).await {
                    Ok(msg) => {
                        tracing::info!(
                            key = %self.key,
                            message = %msg,
                            "force-unlock succeeded"
                        );
                        self.mgr.events.record(
                            &event_ref_helper::stack_ref(stack),
                            &crate::operator::events::StackEvent::ForceUnlocked,
                            stack,
                            &self.mgr.metrics,
                        );
                        self.state.lock_state.on_success();
                        true
                    }
                    Err(e) => {
                        tracing::warn!(
                            key = %self.key,
                            error = %e,
                            "CancelUpdate gRPC failed (lock may clear on its own)"
                        );
                        false
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    key = %self.key,
                    error = %e,
                    "failed to connect to workspace for force-unlock"
                );
                // If workspace is gone, the lock may be orphaned in backend.
                // Delete workspace so it gets recreated on next reconcile.
                if stack.spec.workspace_reclaim_policy == WorkspaceReclaimPolicy::Delete {
                    tracing::info!(
                        key = %self.key,
                        "deleting stuck workspace for recreation"
                    );
                    self.delete_workspace(workspace_name).await;
                }
                false
            }
        }
    }

    /// Delete the Workspace CR (cascade deletes StatefulSet/Service/pods).
    /// Matches Go operator's workspace reclaim policy behavior.
    async fn delete_workspace(&self, stack_name: &str) {
        let ns = self.key.ns.as_str();
        let ws_api: Api<Workspace> = Api::namespaced(self.mgr.client.clone(), ns);
        // Workspace name matches the Stack name
        match ws_api.delete(stack_name, &kube::api::DeleteParams::default()).await {
            Ok(_) => {
                tracing::info!(key = %self.key, "workspace deleted (reclaim policy: Delete)");
            }
            Err(kube::Error::Api(err)) if err.code == 404 => {
                tracing::debug!(key = %self.key, "workspace already deleted");
            }
            Err(e) => {
                tracing::error!(key = %self.key, error = %e, "failed to delete workspace");
            }
        }
    }

    /// Delete the Flux Kustomization that owns this Stack.
    /// Found by walking ownerReferences upward from the Stack.
    async fn delete_owning_kustomization(&self, stack: &Stack) {
        use kube::api::DynamicObject;

        let ns = self.key.ns.as_str();

        // Find the Kustomization ownerReference on the Stack
        let kustomization_ref = stack
            .owner_references()
            .iter()
            .find(|o| o.kind == "Kustomization");

        let ks_name = match kustomization_ref {
            Some(r) => &r.name,
            None => {
                tracing::warn!(
                    key = %self.key,
                    "no Kustomization ownerReference found on Stack, cannot delete"
                );
                return;
            }
        };

        // Use dynamic API to delete the Kustomization (avoids needing the Flux CRD types)
        let api_resource = kube::api::ApiResource {
            group: "kustomize.toolkit.fluxcd.io".to_owned(),
            version: "v1".to_owned(),
            api_version: "kustomize.toolkit.fluxcd.io/v1".to_owned(),
            kind: "Kustomization".to_owned(),
            plural: "kustomizations".to_owned(),
        };
        let ks_api: Api<DynamicObject> =
            Api::namespaced_with(self.mgr.client.clone(), ns, &api_resource);

        match ks_api.delete(ks_name, &kube::api::DeleteParams::default()).await {
            Ok(_) => {
                tracing::info!(
                    key = %self.key,
                    kustomization = %ks_name,
                    "deleted Kustomization (project grace period expired)"
                );
            }
            Err(kube::Error::Api(err)) if err.code == 404 => {
                tracing::debug!(
                    key = %self.key,
                    kustomization = %ks_name,
                    "Kustomization already deleted"
                );
            }
            Err(e) => {
                tracing::error!(
                    key = %self.key,
                    kustomization = %ks_name,
                    error = %e,
                    "failed to delete Kustomization"
                );
            }
        }
    }

    /// Remove the finalizer from an Update CR so it can be garbage-collected.
    async fn remove_update_finalizer(&self, ns: &str, update_name: &str) {
        use kube::api::{Patch, PatchParams};

        let updates: Api<Update> = Api::namespaced(self.mgr.client.clone(), ns);
        let patch = serde_json::json!({
            "metadata": { "finalizers": null }
        });
        if let Err(e) = updates
            .patch(update_name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
        {
            tracing::warn!(key = %self.key, update = %update_name, error = %e, "failed to remove update finalizer");
        }
    }

    /// Clean up orphaned Updates when the Stack has been deleted.
    /// Lists Updates in the namespace and removes finalizers from any owned by this Stack.
    async fn cleanup_orphaned_updates(&self) {
        let ns = self.key.ns.as_str();
        let updates: Api<Update> = Api::namespaced(self.mgr.client.clone(), ns);
        let list = match updates.list(&kube::api::ListParams::default()).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(key = %self.key, error = %e, "failed to list updates for orphan cleanup");
                return;
            }
        };
        for update in list.items {
            let owned_by_this_stack = update
                .owner_references()
                .iter()
                .any(|o| o.kind == "Stack" && o.name == self.key.name.as_str());
            if owned_by_this_stack {
                if let Some(ref finalizers) = update.metadata.finalizers {
                    if finalizers.iter().any(|f| f == STACK_FINALIZER) {
                        let uname = update.name_any();
                        tracing::info!(key = %self.key, update = %uname, "removing finalizer from orphaned update");
                        self.remove_update_finalizer(ns, &uname).await;
                    }
                }
            }
        }
    }

    async fn remove_stack_finalizer(&self, api: &Api<Stack>, name: &str, stack: &Stack) {
        use crate::api::conditions::STACK_FINALIZER;
        use kube::api::{Patch, PatchParams};
        use kube::ResourceExt;

        let mut finalizers: Vec<String> = stack.finalizers().to_vec();
        finalizers.retain(|f| f != STACK_FINALIZER);
        let patch = serde_json::json!({
            "metadata": {
                "finalizers": finalizers
            }
        });
        if let Err(e) = api
            .patch(name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
        {
            tracing::error!(key = %self.key, error = %e, "failed to remove finalizer");
        }
    }

    async fn handle_error(&mut self, error: &OperatorError, stack: &Stack) {
        use crate::core::recovery::RecoveryAction as RA;

        tracing::warn!(key = %self.key, error = %error, "reconcile error");

        match recovery_action(error) {
            RA::RetryWithBackoff { base_ms, max_ms } => {
                self.state.failure_count += 1;
                let delay_ms = base_ms.saturating_mul(
                    2u64.saturating_pow(self.state.failure_count.min(20)),
                ).min(max_ms);
                tracing::info!(
                    key = %self.key,
                    delay_ms,
                    failure_count = self.state.failure_count,
                    "scheduling retry with backoff"
                );
                self.schedule_requeue(
                    Duration::from_millis(delay_ms),
                    Priority::FailureRetry,
                    ReconcileTrigger::Retry,
                );
            }
            RA::Stall => {
                tracing::warn!(key = %self.key, "stack stalled: {}", error);
                self.mgr.events.record(
                    &event_ref_helper::stack_ref(stack),
                    &crate::operator::events::StackEvent::Stalled {
                        reason: error.condition_reason(),
                        message: &error.to_string(),
                    },
                    stack,
                    &self.mgr.metrics,
                );
            }
            RA::ForceUnlockAndRetry => {
                self.mgr.metrics.inc_lock_conflicts();
                match self.state.lock_state.on_conflict() {
                    LockAction::ForceUnlock => {
                        tracing::warn!(
                            key = %self.key,
                            "lock timeout exceeded in pipeline, scheduling force-unlock"
                        );
                        self.mgr.metrics.inc_force_unlocks();
                        self.schedule_requeue(
                            Duration::from_secs(2),
                            Priority::LockRecovery,
                            ReconcileTrigger::LockRetry,
                        );
                    }
                    LockAction::RetryAfter(d) => {
                        tracing::info!(
                            key = %self.key,
                            delay_secs = d.as_secs(),
                            "lock conflict in pipeline, scheduling retry"
                        );
                        self.schedule_requeue(
                            d,
                            Priority::FailureRetry,
                            ReconcileTrigger::Retry,
                        );
                    }
                    LockAction::Clear => {
                        self.state.lock_state.on_success();
                    }
                }
            }
        }
    }
}

use crate::core::lock::is_lock_error;

/// Helper to create an ObjectReference from a Stack for event emission.
mod event_ref_helper {
    use k8s_openapi::api::core::v1::ObjectReference;
    use kube::ResourceExt;
    use crate::api::stack::Stack;

    pub fn stack_ref(stack: &Stack) -> ObjectReference {
        ObjectReference {
            api_version: Some("pulumi.com/v1".to_owned()),
            kind: Some("Stack".to_owned()),
            name: Some(stack.name_any()),
            namespace: stack.namespace(),
            uid: stack.uid(),
            ..Default::default()
        }
    }
}
