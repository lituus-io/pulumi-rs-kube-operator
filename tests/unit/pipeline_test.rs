use pulumi_kubernetes_operator::operator::reconcile::pipeline::ReconcileAction;

#[test]
fn reconcile_action_debug_output() {
    let actions: Vec<ReconcileAction> = vec![
        ReconcileAction::AddFinalizer,
        ReconcileAction::Done,
        ReconcileAction::RemoveFinalizer,
        ReconcileAction::WaitForUpdate { name: "u1".into() },
        ReconcileAction::Synced,
        ReconcileAction::WaitForWorkspace,
        ReconcileAction::UpdateCreated { name: "u2".into() },
        ReconcileAction::DestroyStarted { name: "d1".into() },
        ReconcileAction::DestroyFailed {
            name: "d2".into(),
            failures: 3,
        },
        ReconcileAction::RemoveFinalizerAfterDestroy,
        ReconcileAction::UpdateSucceeded {
            name: "u3".into(),
            permalink: Some("https://app.pulumi.com/foo".into()),
            outputs: None,
        },
        ReconcileAction::UpdateFailed {
            name: "u4".into(),
            message: "boom".into(),
        },
    ];

    assert_eq!(actions.len(), 12, "must have all 12 variants");
    for action in &actions {
        let dbg = format!("{:?}", action);
        assert!(!dbg.is_empty(), "Debug output must be non-empty");
    }
}

#[test]
fn reconcile_action_variant_fields() {
    // Verify field access on data-carrying variants
    let action = ReconcileAction::WaitForUpdate {
        name: "test-update".into(),
    };
    if let ReconcileAction::WaitForUpdate { name } = &action {
        assert_eq!(name, "test-update");
    } else {
        panic!("expected WaitForUpdate");
    }

    let action = ReconcileAction::DestroyFailed {
        name: "d".into(),
        failures: 5,
    };
    if let ReconcileAction::DestroyFailed { name, failures } = &action {
        assert_eq!(name, "d");
        assert_eq!(*failures, 5);
    } else {
        panic!("expected DestroyFailed");
    }

    let action = ReconcileAction::UpdateSucceeded {
        name: "u".into(),
        permalink: None,
        outputs: Some(r#"{"key":"val"}"#.into()),
    };
    if let ReconcileAction::UpdateSucceeded {
        name,
        permalink,
        outputs,
    } = &action
    {
        assert_eq!(name, "u");
        assert!(permalink.is_none());
        assert!(outputs.is_some());
    } else {
        panic!("expected UpdateSucceeded");
    }
}

#[test]
fn reconcile_action_update_failed_message() {
    let action = ReconcileAction::UpdateFailed {
        name: "upd-abc".into(),
        message: "exit code 1".into(),
    };
    if let ReconcileAction::UpdateFailed { name, message } = &action {
        assert_eq!(name, "upd-abc");
        assert!(message.contains("exit code"));
    } else {
        panic!("expected UpdateFailed");
    }
}
