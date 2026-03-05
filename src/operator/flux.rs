use k8s_openapi::api::core::v1::ObjectReference;
use kube::runtime::events::{Event, EventType, Recorder, Reporter};

use crate::errors::OperatorError;

pub struct FluxEventEmitter {
    reporter: Reporter,
    client: kube::Client,
}

impl FluxEventEmitter {
    pub fn new(client: kube::Client) -> Self {
        Self {
            reporter: Reporter {
                controller: "pulumi-kubernetes-operator".into(),
                instance: std::env::var("POD_NAME").ok(),
            },
            client,
        }
    }

    /// Emit Warning event for permanent errors. Flux notification-controller
    /// watches for Warning events on Stacks matching Alert selectors.
    /// Only called for errors where should_notify() == true (permanent only).
    pub async fn emit(&self, stack_ref: &ObjectReference, error: &OperatorError) {
        if !error.should_notify() {
            return;
        }

        let recorder = Recorder::new(self.client.clone(), self.reporter.clone());

        let result = recorder
            .publish(
                &Event {
                    type_: EventType::Warning,
                    reason: error.condition_reason().into(),
                    note: Some(error.to_string()),
                    action: "Reconcile".into(),
                    secondary: None,
                },
                stack_ref,
            )
            .await;

        if let Err(e) = result {
            tracing::warn!(error = %e, "failed to emit Flux event");
        }
    }
}
