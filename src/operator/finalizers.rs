use kube::Resource;

use crate::api::conditions::{PROGRAM_FINALIZER, STACK_FINALIZER};
use crate::api::program::Program;
use crate::api::stack::Stack;
use crate::api::update::{Update, UpdateSpec};
use crate::core::finalizer::has_finalizer;

/// Determines what finalizer action to take for a Stack.
/// Pure function -- no I/O, no allocation.
pub fn stack_finalizer_action(stack: &Stack) -> StackFinalizerAction {
    let has = has_finalizer(stack, STACK_FINALIZER);
    let deleting = stack.meta().deletion_timestamp.is_some();
    let destroy = stack.spec.destroy_on_finalize;
    let preview = stack.spec.preview;

    match (has, deleting, destroy, preview) {
        (false, false, _, _) => StackFinalizerAction::Add,
        (true, false, _, _) => StackFinalizerAction::None,
        (false, true, _, _) => StackFinalizerAction::AlreadyFinalized,
        (true, true, _, true) => StackFinalizerAction::RemoveImmediately, // Preview: no destroy
        (true, true, false, _) => StackFinalizerAction::RemoveImmediately,
        (true, true, true, false) => StackFinalizerAction::RunDestroy,
    }
}

pub enum StackFinalizerAction {
    Add,
    None,
    AlreadyFinalized,
    RemoveImmediately,
    RunDestroy,
}

/// Program protection -- prevents deletion while referenced by any Stack.
/// Takes a count instead of a slice to avoid cloning Arc<Stack> from the store.
pub fn program_finalizer_action(
    program: &Program,
    referencing_count: usize,
) -> ProgramFinalizerAction {
    let has = has_finalizer(program, PROGRAM_FINALIZER);
    let deleting = program.meta().deletion_timestamp.is_some();
    let no_refs = referencing_count == 0;

    match (has, deleting, no_refs) {
        (false, false, false) => ProgramFinalizerAction::Add, // Stacks reference it
        (false, _, true) => ProgramFinalizerAction::None,     // No references
        (true, true, true) => ProgramFinalizerAction::Remove, // Safe to delete
        (true, true, false) => ProgramFinalizerAction::Block, // Still referenced
        _ => ProgramFinalizerAction::None,
    }
}

pub enum ProgramFinalizerAction {
    Add,
    None,
    Remove,
    Block,
}

/// Build an Update object with the finalizer already set.
/// Single API call = atomic, no race window.
pub fn build_update_with_finalizer(
    name: &str,
    namespace: &str,
    spec: UpdateSpec,
    owner_ref: k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference,
) -> Update {
    let mut update = Update::new(name, spec);
    update.metadata.namespace = Some(namespace.to_owned());
    update.metadata.finalizers = Some(vec![STACK_FINALIZER.to_owned()]);
    update.metadata.owner_references = Some(vec![owner_ref]);
    update
}

// has_finalizer is now generic in core::finalizer -- works on any ResourceExt.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::stack::StackSpec;

    fn make_stack(has_finalizer: bool, deleting: bool, destroy: bool, preview: bool) -> Stack {
        let mut stack = Stack::new("test", StackSpec {
            stack: "org/test".into(),
            destroy_on_finalize: destroy,
            preview,
            ..default_stack_spec()
        });
        if has_finalizer {
            stack.metadata.finalizers = Some(vec![STACK_FINALIZER.to_owned()]);
        }
        if deleting {
            stack.metadata.deletion_timestamp =
                Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                    chrono::Utc::now(),
                ));
        }
        stack
    }

    fn default_stack_spec() -> StackSpec {
        serde_json::from_str(r#"{"stack": "org/test"}"#).unwrap()
    }

    #[test]
    fn no_finalizer_not_deleting_adds() {
        let stack = make_stack(false, false, false, false);
        assert!(matches!(
            stack_finalizer_action(&stack),
            StackFinalizerAction::Add
        ));
    }

    #[test]
    fn has_finalizer_not_deleting_noop() {
        let stack = make_stack(true, false, false, false);
        assert!(matches!(
            stack_finalizer_action(&stack),
            StackFinalizerAction::None
        ));
    }

    #[test]
    fn no_finalizer_deleting_already_finalized() {
        let stack = make_stack(false, true, false, false);
        assert!(matches!(
            stack_finalizer_action(&stack),
            StackFinalizerAction::AlreadyFinalized
        ));
    }

    #[test]
    fn deleting_without_destroy_removes() {
        let stack = make_stack(true, true, false, false);
        assert!(matches!(
            stack_finalizer_action(&stack),
            StackFinalizerAction::RemoveImmediately
        ));
    }

    #[test]
    fn deleting_with_destroy_runs_destroy() {
        let stack = make_stack(true, true, true, false);
        assert!(matches!(
            stack_finalizer_action(&stack),
            StackFinalizerAction::RunDestroy
        ));
    }

    #[test]
    fn deleting_with_destroy_but_preview_removes() {
        let stack = make_stack(true, true, true, true);
        assert!(matches!(
            stack_finalizer_action(&stack),
            StackFinalizerAction::RemoveImmediately
        ));
    }
}
