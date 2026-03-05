/// Helpers for building Stack status SSA patches.
/// Reduces boilerplate in actor.rs where 8+ status patches share the same outer structure.
///
/// Wrap a status object in the full Stack patch structure for SSA.
pub fn stack_patch(status: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "pulumi.com/v1",
        "kind": "Stack",
        "status": status,
    })
}

/// Build a single Kubernetes condition object.
pub fn condition(
    r#type: &str,
    status: &str,
    reason: &str,
    message: impl Into<String>,
    now: &str,
    generation: i64,
) -> serde_json::Value {
    serde_json::json!({
        "type": r#type,
        "status": status,
        "reason": reason,
        "message": message.into(),
        "lastTransitionTime": now,
        "observedGeneration": generation,
    })
}
