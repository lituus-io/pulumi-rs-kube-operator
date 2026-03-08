use kube::runtime::reflector::Store;

use crate::api::stack::{PrerequisiteRef, Stack};
use crate::core::time::{elapsed_since, parse_go_duration};
use crate::errors::{OperatorError, TransientError};

/// Check all prerequisites using the shared informer store.
/// Synchronous Store lookups -- no async API calls needed.
pub fn check_prerequisites(
    store: &Store<Stack>,
    ns: &str,
    prerequisites: &[PrerequisiteRef],
) -> Result<(), OperatorError> {
    if prerequisites.is_empty() {
        return Ok(());
    }

    for prereq in prerequisites {
        check_single_prerequisite(store, ns, prereq)?;
    }

    Ok(())
}

fn check_single_prerequisite(
    store: &Store<Stack>,
    ns: &str,
    prereq: &PrerequisiteRef,
) -> Result<(), OperatorError> {
    // Look up the prerequisite stack from the shared cache
    let key = kube::runtime::reflector::ObjectRef::new(&prereq.name).within(ns);
    let stack = store.get(&key).ok_or(OperatorError::Transient(
        TransientError::PrerequisiteNotSatisfied,
    ))?;

    let status = stack
        .status
        .as_ref()
        .and_then(|s| s.last_update.as_ref())
        .ok_or(OperatorError::Transient(
            TransientError::PrerequisiteNotSatisfied,
        ))?;

    // Check the prerequisite stack has succeeded
    if status.state.as_deref() != Some("succeeded") {
        return Err(OperatorError::Transient(
            TransientError::PrerequisiteNotSatisfied,
        ));
    }

    // Check succeededWithinDuration if specified
    if let Some(ref req) = prereq.requirement {
        if let Some(ref duration_str) = req.succeeded_within_duration {
            if let Some(ref resync_time) = status.last_resync_time {
                let within = parse_go_duration(duration_str);
                let elapsed = elapsed_since(Some(resync_time));
                if elapsed > within {
                    return Err(OperatorError::Transient(
                        TransientError::PrerequisiteNotSatisfied,
                    ));
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::stack::{RequirementSpec, StackSpec, StackStatus, StackUpdateState};
    use kube::runtime::reflector;
    use kube::runtime::watcher;

    fn default_spec() -> StackSpec {
        serde_json::from_str(r#"{"stack": "org/test"}"#).unwrap()
    }

    fn make_stack(name: &str, ns: &str, status: Option<StackStatus>) -> Stack {
        let mut stack = Stack::new(name, default_spec());
        stack.metadata.namespace = Some(ns.to_owned());
        stack.status = status;
        stack
    }

    fn succeeded_status(resync_time: Option<&str>) -> StackStatus {
        StackStatus {
            last_update: Some(StackUpdateState {
                state: Some("succeeded".into()),
                last_resync_time: resync_time.map(String::from),
                ..default_update_state()
            }),
            ..Default::default()
        }
    }

    fn default_update_state() -> StackUpdateState {
        StackUpdateState {
            generation: 0,
            reconcile_request: None,
            name: None,
            update_type: None,
            state: None,
            message: None,
            last_attempted_commit: None,
            last_successful_commit: None,
            permalink: None,
            last_resync_time: None,
            failures: 0,
        }
    }

    fn build_store(stacks: Vec<Stack>) -> Store<Stack> {
        let (store, mut writer) = reflector::store();
        writer.apply_watcher_event(&watcher::Event::Init);
        for s in &stacks {
            writer.apply_watcher_event(&watcher::Event::InitApply(s.clone()));
        }
        writer.apply_watcher_event(&watcher::Event::InitDone);
        store
    }

    #[test]
    fn empty_prerequisites_ok() {
        let store = build_store(vec![]);
        assert!(check_prerequisites(&store, "default", &[]).is_ok());
    }

    #[test]
    fn missing_prerequisite_errors() {
        let store = build_store(vec![]);
        let prereqs = vec![PrerequisiteRef {
            name: "missing".into(),
            requirement: None,
        }];
        assert!(check_prerequisites(&store, "default", &prereqs).is_err());
    }

    #[test]
    fn succeeded_prerequisite_ok() {
        let stack = make_stack("dep", "default", Some(succeeded_status(None)));
        let store = build_store(vec![stack]);
        let prereqs = vec![PrerequisiteRef {
            name: "dep".into(),
            requirement: None,
        }];
        assert!(check_prerequisites(&store, "default", &prereqs).is_ok());
    }

    #[test]
    fn failed_prerequisite_errors() {
        let status = StackStatus {
            last_update: Some(StackUpdateState {
                state: Some("failed".into()),
                ..default_update_state()
            }),
            ..Default::default()
        };
        let stack = make_stack("dep", "default", Some(status));
        let store = build_store(vec![stack]);
        let prereqs = vec![PrerequisiteRef {
            name: "dep".into(),
            requirement: None,
        }];
        assert!(check_prerequisites(&store, "default", &prereqs).is_err());
    }

    #[test]
    fn no_status_prerequisite_errors() {
        let stack = make_stack("dep", "default", None);
        let store = build_store(vec![stack]);
        let prereqs = vec![PrerequisiteRef {
            name: "dep".into(),
            requirement: None,
        }];
        assert!(check_prerequisites(&store, "default", &prereqs).is_err());
    }

    #[test]
    fn succeeded_within_duration_recent_ok() {
        let now = chrono::Utc::now().to_rfc3339();
        let stack = make_stack("dep", "default", Some(succeeded_status(Some(&now))));
        let store = build_store(vec![stack]);
        let prereqs = vec![PrerequisiteRef {
            name: "dep".into(),
            requirement: Some(RequirementSpec {
                succeeded_within_duration: Some("1h".into()),
            }),
        }];
        assert!(check_prerequisites(&store, "default", &prereqs).is_ok());
    }

    #[test]
    fn succeeded_within_duration_stale_errors() {
        let stale = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        let stack = make_stack("dep", "default", Some(succeeded_status(Some(&stale))));
        let store = build_store(vec![stack]);
        let prereqs = vec![PrerequisiteRef {
            name: "dep".into(),
            requirement: Some(RequirementSpec {
                succeeded_within_duration: Some("1h".into()),
            }),
        }];
        assert!(check_prerequisites(&store, "default", &prereqs).is_err());
    }

    #[test]
    fn multiple_prerequisites_all_must_pass() {
        let s1 = make_stack("dep1", "default", Some(succeeded_status(None)));
        let s2 = make_stack("dep2", "default", Some(succeeded_status(None)));
        let store = build_store(vec![s1, s2]);
        let prereqs = vec![
            PrerequisiteRef {
                name: "dep1".into(),
                requirement: None,
            },
            PrerequisiteRef {
                name: "dep2".into(),
                requirement: None,
            },
        ];
        assert!(check_prerequisites(&store, "default", &prereqs).is_ok());

        // If one is missing, should fail
        let prereqs_with_missing = vec![
            PrerequisiteRef {
                name: "dep1".into(),
                requirement: None,
            },
            PrerequisiteRef {
                name: "missing".into(),
                requirement: None,
            },
        ];
        assert!(check_prerequisites(&store, "default", &prereqs_with_missing).is_err());
    }
}
