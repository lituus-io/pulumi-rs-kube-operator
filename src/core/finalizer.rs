use kube::ResourceExt;

/// Generic finalizer presence check -- works on any kube resource.
pub fn has_finalizer<T: ResourceExt>(obj: &T, name: &str) -> bool {
    obj.finalizers().iter().any(|s| s == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::conditions::{PROGRAM_FINALIZER, STACK_FINALIZER};
    use crate::api::program::Program;
    use crate::api::stack::{Stack, StackSpec};

    fn default_spec() -> StackSpec {
        serde_json::from_str(r#"{"stack": "org/test"}"#).unwrap()
    }

    #[test]
    fn has_finalizer_on_stack() {
        let mut stack = Stack::new("test", default_spec());
        assert!(!has_finalizer(&stack, STACK_FINALIZER));

        stack.metadata.finalizers = Some(vec![STACK_FINALIZER.to_owned()]);
        assert!(has_finalizer(&stack, STACK_FINALIZER));
    }

    #[test]
    fn has_finalizer_on_program() {
        let spec: crate::api::program::ProgramSpec = serde_json::from_str(r#"{}"#).unwrap();
        let mut program = Program::new("test", spec);
        assert!(!has_finalizer(&program, PROGRAM_FINALIZER));

        program.metadata.finalizers = Some(vec![PROGRAM_FINALIZER.to_owned()]);
        assert!(has_finalizer(&program, PROGRAM_FINALIZER));
    }
}
