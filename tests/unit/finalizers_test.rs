use pulumi_kubernetes_operator::api::conditions::{PROGRAM_FINALIZER, STACK_FINALIZER};
use pulumi_kubernetes_operator::api::program::{Program, ProgramSpec};
use pulumi_kubernetes_operator::api::update::UpdateSpec;
use pulumi_kubernetes_operator::operator::finalizers::{
    build_update_with_finalizer, program_finalizer_action, ProgramFinalizerAction,
};

fn make_program(has_finalizer: bool, deleting: bool) -> Program {
    let spec: ProgramSpec = serde_json::from_str(r#"{}"#).unwrap();
    let mut program = Program::new("test-program", spec);
    if has_finalizer {
        program.metadata.finalizers = Some(vec![PROGRAM_FINALIZER.to_owned()]);
    }
    if deleting {
        program.metadata.deletion_timestamp =
            Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                chrono::Utc::now(),
            ));
    }
    program
}

// --- program_finalizer_action tests ---

#[test]
fn program_no_finalizer_not_deleting_with_refs_adds() {
    let program = make_program(false, false);

    assert!(matches!(
        program_finalizer_action(&program, 1),
        ProgramFinalizerAction::Add
    ));
}

#[test]
fn program_no_finalizer_no_refs_noop() {
    let program = make_program(false, false);

    assert!(matches!(
        program_finalizer_action(&program, 0),
        ProgramFinalizerAction::None
    ));
}

#[test]
fn program_has_finalizer_deleting_no_refs_removes() {
    let program = make_program(true, true);

    assert!(matches!(
        program_finalizer_action(&program, 0),
        ProgramFinalizerAction::Remove
    ));
}

#[test]
fn program_has_finalizer_deleting_with_refs_blocks() {
    let program = make_program(true, true);

    assert!(matches!(
        program_finalizer_action(&program, 1),
        ProgramFinalizerAction::Block
    ));
}

#[test]
fn program_has_finalizer_not_deleting_noop() {
    let program = make_program(true, false);

    assert!(matches!(
        program_finalizer_action(&program, 0),
        ProgramFinalizerAction::None
    ));
}

#[test]
fn program_no_finalizer_deleting_no_refs_noop() {
    let program = make_program(false, true);

    assert!(matches!(
        program_finalizer_action(&program, 0),
        ProgramFinalizerAction::None
    ));
}

// --- build_update_with_finalizer tests ---

#[test]
fn build_update_sets_namespace_owner_finalizer() {
    use kube::ResourceExt;

    let spec = UpdateSpec {
        workspace_name: Some("ws".to_owned()),
        stack_name: Some("org/stack".to_owned()),
        update_type: None,
        ttl_after_completed: None,
        parallel: None,
        message: None,
        expect_no_changes: None,
        replace: vec![],
        target: vec![],
        target_dependents: None,
        refresh: None,
        continue_on_error: None,
        remove: None,
    };

    let owner_ref = k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference {
        api_version: "pulumi.com/v1".to_owned(),
        kind: "Stack".to_owned(),
        name: "my-stack".to_owned(),
        uid: "abc-123".to_owned(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    };

    let update = build_update_with_finalizer("test-update", "test-ns", spec, owner_ref);

    // Verify namespace
    assert_eq!(update.metadata.namespace.as_deref(), Some("test-ns"));

    // Verify name
    assert_eq!(update.name_any(), "test-update");

    // Verify finalizer
    let finalizers = update.metadata.finalizers.as_ref().unwrap();
    assert_eq!(finalizers.len(), 1);
    assert_eq!(finalizers[0], STACK_FINALIZER);

    // Verify owner reference
    let owners = update.metadata.owner_references.as_ref().unwrap();
    assert_eq!(owners.len(), 1);
    assert_eq!(owners[0].name, "my-stack");
    assert_eq!(owners[0].kind, "Stack");
    assert_eq!(owners[0].uid, "abc-123");
}
