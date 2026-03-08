use k8s_openapi::api::core::v1::ObjectReference;
use kube::runtime::events::{Event, EventType, Recorder, Reporter};

use crate::api::conditions::*;
use crate::api::stack::{NotificationEventFilter, NotificationWebhook, Stack};
use crate::operator::metrics::Metrics;

/// Severity of a Kubernetes Event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Normal,
    Warning,
}

impl Severity {
    const fn as_event_type(self) -> EventType {
        match self {
            Severity::Normal => EventType::Normal,
            Severity::Warning => EventType::Warning,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Severity::Normal => "Normal",
            Severity::Warning => "Warning",
        }
    }
}

/// All significant lifecycle events for a Stack. Borrows data from the caller.
pub enum StackEvent<'a> {
    UpdateCreated {
        update_name: &'a str,
    },
    UpdateSucceeded {
        update_name: &'a str,
        permalink: Option<&'a str>,
    },
    UpdateFailed {
        update_name: &'a str,
        message: &'a str,
    },
    LockConflict {
        update_name: &'a str,
    },
    DestroyStarted {
        update_name: &'a str,
    },
    DestroyFailed {
        update_name: &'a str,
        attempt: i64,
    },
    DestroySucceeded,
    WorkspaceDeleted,
    ForceUnlocked,
    Stalled {
        reason: &'static str,
        message: &'a str,
    },
    ProjectNotFound {
        project_id: &'a str,
    },
    ProjectTtlExpired,
}

impl<'a> StackEvent<'a> {
    /// Returns the Kubernetes Event reason string (&'static str, zero-alloc).
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::UpdateCreated { .. } => EVT_UPDATE_DETECTED,
            Self::UpdateSucceeded { .. } => EVT_UPDATE_SUCCESS,
            Self::UpdateFailed { .. } => EVT_UPDATE_FAILED,
            Self::LockConflict { .. } => EVT_CONFLICT_DETECTED,
            Self::DestroyStarted { .. } => EVT_UPDATE_DETECTED,
            Self::DestroyFailed { .. } => EVT_UPDATE_FAILURE,
            Self::DestroySucceeded => EVT_DESTROY_SUCCESS,
            Self::WorkspaceDeleted => EVT_WORKSPACE_DELETED,
            Self::ForceUnlocked => EVT_LOCK_UNLOCKED,
            Self::Stalled { .. } => STALLED_SPEC_INVALID,
            Self::ProjectNotFound { .. } => PENDING_DELETION_PROJECT,
            Self::ProjectTtlExpired => PENDING_DELETION_TTL_EXPIRED,
        }
    }

    /// Returns Normal for success/informational events, Warning for failures.
    pub const fn severity(&self) -> Severity {
        match self {
            Self::UpdateCreated { .. }
            | Self::UpdateSucceeded { .. }
            | Self::DestroyStarted { .. }
            | Self::DestroySucceeded
            | Self::WorkspaceDeleted => Severity::Normal,
            Self::UpdateFailed { .. }
            | Self::LockConflict { .. }
            | Self::DestroyFailed { .. }
            | Self::ForceUnlocked
            | Self::Stalled { .. }
            | Self::ProjectNotFound { .. }
            | Self::ProjectTtlExpired => Severity::Warning,
        }
    }

    /// Human-readable note for the Kubernetes Event. Only allocation point.
    pub fn note(&self) -> String {
        match self {
            Self::UpdateCreated { update_name } => {
                format!("Update {} created", update_name)
            }
            Self::UpdateSucceeded {
                update_name,
                permalink,
            } => {
                if let Some(link) = permalink {
                    format!("Update {} succeeded ({})", update_name, link)
                } else {
                    format!("Update {} succeeded", update_name)
                }
            }
            Self::UpdateFailed {
                update_name,
                message,
            } => {
                format!("Update {} failed: {}", update_name, message)
            }
            Self::LockConflict { update_name } => {
                format!("Update {} blocked by lock conflict", update_name)
            }
            Self::DestroyStarted { update_name } => {
                format!("Destroy {} started", update_name)
            }
            Self::DestroyFailed {
                update_name,
                attempt,
            } => {
                format!("Destroy {} failed (attempt {})", update_name, attempt)
            }
            Self::DestroySucceeded => "Stack destroyed successfully".to_owned(),
            Self::WorkspaceDeleted => "Workspace deleted".to_owned(),
            Self::ForceUnlocked => "Backend lock force-unlocked via CancelUpdate".to_owned(),
            Self::Stalled { reason, message } => {
                format!("Stack stalled ({}): {}", reason, message)
            }
            Self::ProjectNotFound { project_id } => {
                format!("Project {} not found, grace period started", project_id)
            }
            Self::ProjectTtlExpired => "Project grace period expired, executing cleanup".to_owned(),
        }
    }

    /// Maps terminal events to their notification filter variant.
    /// Returns None for intermediate events that should not trigger webhooks.
    pub const fn notification_filter(&self) -> Option<NotificationEventFilter> {
        match self {
            Self::UpdateSucceeded { .. } => Some(NotificationEventFilter::UpdateSucceeded),
            Self::UpdateFailed { .. } => Some(NotificationEventFilter::UpdateFailed),
            Self::DestroySucceeded => Some(NotificationEventFilter::DestroySucceeded),
            Self::DestroyFailed { .. } => Some(NotificationEventFilter::DestroyFailed),
            Self::LockConflict { .. } => Some(NotificationEventFilter::LockConflict),
            Self::Stalled { .. } => Some(NotificationEventFilter::Stalled),
            // Intermediate events: no webhook
            Self::UpdateCreated { .. }
            | Self::DestroyStarted { .. }
            | Self::WorkspaceDeleted
            | Self::ForceUnlocked
            | Self::ProjectNotFound { .. }
            | Self::ProjectTtlExpired => None,
        }
    }
}

/// Unified event recorder. Emits Kubernetes Events and dispatches notification webhooks.
/// Lives on `&'static Manager` — no Arc.
pub struct EventRecorder {
    reporter: Reporter,
    client: kube::Client,
    http_client: reqwest::Client,
}

impl EventRecorder {
    pub fn new(client: kube::Client) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("failed to create HTTP client for webhooks");
        Self {
            reporter: Reporter {
                controller: "pulumi-kubernetes-operator".into(),
                instance: std::env::var("POD_NAME").ok(),
            },
            client,
            http_client,
        }
    }

    /// Record a Kubernetes Event and dispatch matching notification webhooks.
    /// Fire-and-forget: spawned tasks never block reconciliation.
    pub fn record(
        &'static self,
        stack_ref: &ObjectReference,
        event: &StackEvent<'_>,
        stack: &Stack,
        metrics: &'static Metrics,
    ) {
        metrics.inc_events_emitted();

        // Build the K8s Event
        let k8s_event = Event {
            type_: event.severity().as_event_type(),
            reason: event.reason().into(),
            note: Some(event.note()),
            action: "Reconcile".into(),
            secondary: None,
        };
        let recorder = Recorder::new(self.client.clone(), self.reporter.clone());
        let obj_ref = stack_ref.clone();
        tokio::spawn(async move {
            if let Err(e) = recorder.publish(&k8s_event, &obj_ref).await {
                tracing::warn!(error = %e, "failed to publish Kubernetes event");
            }
        });

        // Dispatch notification webhooks
        if let Some(filter) = event.notification_filter() {
            let notifications = &stack.spec.notifications;
            if !notifications.is_empty() {
                let payload = build_payload(stack, event);
                for webhook in notifications {
                    if webhook.events.is_empty() || webhook.events.contains(&filter) {
                        self.spawn_webhook(webhook, &payload, metrics);
                    }
                }
            }
        }
    }

    fn spawn_webhook(
        &'static self,
        webhook: &NotificationWebhook,
        payload: &serde_json::Value,
        metrics: &'static Metrics,
    ) {
        let url = webhook.url.clone();
        let payload = payload.clone();
        let auth_secret = webhook.auth_secret.clone();
        tokio::spawn(async move {
            self.dispatch_webhook(&url, &payload, auth_secret.as_ref(), metrics)
                .await;
        });
    }

    async fn dispatch_webhook(
        &self,
        url: &str,
        payload: &serde_json::Value,
        auth_secret: Option<&crate::api::stack::WebhookAuthSecret>,
        metrics: &'static Metrics,
    ) {
        let token = if let Some(secret_ref) = auth_secret {
            match self.resolve_secret(&secret_ref.name, &secret_ref.key).await {
                Ok(t) => Some(t),
                Err(e) => {
                    tracing::warn!(url, error = %e, "failed to resolve webhook auth secret");
                    metrics.inc_notifications_failed();
                    return;
                }
            }
        } else {
            None
        };

        for attempt in 0..2u8 {
            let mut req = self.http_client.post(url).json(payload);
            if let Some(ref t) = token {
                req = req.bearer_auth(t);
            }
            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    metrics.inc_notifications_sent();
                    return;
                }
                Ok(resp) => {
                    tracing::warn!(
                        url, status = %resp.status(), attempt,
                        "webhook returned non-success status"
                    );
                }
                Err(e) => {
                    tracing::warn!(url, error = %e, attempt, "webhook request failed");
                }
            }
            if attempt == 0 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        metrics.inc_notifications_failed();
    }

    async fn resolve_secret(&self, name: &str, key: &str) -> Result<String, kube::Error> {
        use k8s_openapi::api::core::v1::Secret;
        use kube::Api;

        // Resolve in the operator's own namespace (from downward API)
        let ns = std::env::var("POD_NAMESPACE").unwrap_or_else(|_| "default".into());
        let secrets: Api<Secret> = Api::namespaced(self.client.clone(), &ns);
        let secret = secrets.get(name).await?;
        let data = secret.data.unwrap_or_default();
        let bytes = data.get(key).ok_or_else(|| {
            kube::Error::Api(kube::core::ErrorResponse {
                code: 404,
                message: format!("key '{}' not found in secret '{}'", key, name),
                reason: "NotFound".into(),
                status: "Failure".into(),
            })
        })?;
        Ok(String::from_utf8_lossy(&bytes.0).into_owned())
    }
}

fn build_payload(stack: &Stack, event: &StackEvent<'_>) -> serde_json::Value {
    use kube::ResourceExt;

    let mut payload = serde_json::json!({
        "version": "v1",
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "stack": {
            "name": stack.name_any(),
            "namespace": stack.namespace().unwrap_or_default(),
            "stackName": stack.spec.stack,
        },
        "event": {
            "type": event.reason(),
            "reason": event.reason(),
            "message": event.note(),
            "severity": event.severity().as_str(),
        },
    });

    // Add update info for update-related events
    match event {
        StackEvent::UpdateCreated { update_name }
        | StackEvent::UpdateFailed { update_name, .. }
        | StackEvent::LockConflict { update_name }
        | StackEvent::DestroyStarted { update_name }
        | StackEvent::DestroyFailed { update_name, .. } => {
            payload["update"] = serde_json::json!({ "name": update_name });
        }
        StackEvent::UpdateSucceeded {
            update_name,
            permalink,
        } => {
            payload["update"] = serde_json::json!({
                "name": update_name,
                "permalink": permalink,
            });
        }
        _ => {}
    }

    payload
}
