use pulumi_kubernetes_operator::operator::status::{condition, stack_patch};

#[test]
fn stack_patch_wraps_with_api_version_and_kind() {
    let status = serde_json::json!({ "observedGeneration": 1 });
    let patch = stack_patch(status);
    assert_eq!(patch["apiVersion"], "pulumi.com/v1");
    assert_eq!(patch["kind"], "Stack");
    assert_eq!(patch["status"]["observedGeneration"], 1);
}

#[test]
fn condition_produces_all_fields() {
    let c = condition(
        "Ready",
        "True",
        "Completed",
        "All good",
        "2024-01-01T00:00:00Z",
        5,
    );
    assert_eq!(c["type"], "Ready");
    assert_eq!(c["status"], "True");
    assert_eq!(c["reason"], "Completed");
    assert_eq!(c["message"], "All good");
    assert_eq!(c["lastTransitionTime"], "2024-01-01T00:00:00Z");
    assert_eq!(c["observedGeneration"], 5);
}

#[test]
fn condition_with_format_string_message() {
    let name = "my-update";
    let c = condition(
        "Reconciling",
        "True",
        "Processing",
        format!("Update {} in progress", name),
        "2024-01-01T00:00:00Z",
        1,
    );
    assert_eq!(c["message"], "Update my-update in progress");
}

#[test]
fn stack_patch_with_conditions_array() {
    let patch = stack_patch(serde_json::json!({
        "observedGeneration": 3,
        "conditions": [
            condition("Ready", "True", "Ok", "done", "now", 3),
            condition("Reconciling", "False", "Ok", "", "now", 3),
        ]
    }));
    let conditions = patch["status"]["conditions"].as_array().unwrap();
    assert_eq!(conditions.len(), 2);
    assert_eq!(conditions[0]["type"], "Ready");
    assert_eq!(conditions[1]["type"], "Reconciling");
}
