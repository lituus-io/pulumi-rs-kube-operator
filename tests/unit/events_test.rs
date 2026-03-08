use pulumi_kubernetes_operator::api::conditions::*;
use pulumi_kubernetes_operator::api::stack::NotificationEventFilter;
use pulumi_kubernetes_operator::operator::events::{Severity, StackEvent};

#[test]
fn event_reasons_are_static_str() {
    let cases: Vec<(StackEvent<'_>, &str)> = vec![
        (StackEvent::UpdateCreated { update_name: "u1" }, EVT_UPDATE_DETECTED),
        (StackEvent::UpdateSucceeded { update_name: "u1", permalink: None }, EVT_UPDATE_SUCCESS),
        (StackEvent::UpdateFailed { update_name: "u1", message: "err" }, EVT_UPDATE_FAILED),
        (StackEvent::LockConflict { update_name: "u1" }, EVT_CONFLICT_DETECTED),
        (StackEvent::DestroyStarted { update_name: "u1" }, EVT_UPDATE_DETECTED),
        (StackEvent::DestroyFailed { update_name: "u1", attempt: 2 }, EVT_UPDATE_FAILURE),
        (StackEvent::DestroySucceeded, EVT_DESTROY_SUCCESS),
        (StackEvent::WorkspaceDeleted, EVT_WORKSPACE_DELETED),
        (StackEvent::ForceUnlocked, EVT_LOCK_UNLOCKED),
        (StackEvent::Stalled { reason: "SpecInvalid", message: "bad" }, STALLED_SPEC_INVALID),
        (StackEvent::ProjectNotFound { project_id: "p1" }, PENDING_DELETION_PROJECT),
        (StackEvent::ProjectTtlExpired, PENDING_DELETION_TTL_EXPIRED),
    ];

    for (event, expected) in &cases {
        assert_eq!(event.reason(), *expected, "reason mismatch for {:?}", event.note());
    }
}

#[test]
fn severity_normal_for_success_events() {
    let normals: Vec<StackEvent<'_>> = vec![
        StackEvent::UpdateCreated { update_name: "u1" },
        StackEvent::UpdateSucceeded { update_name: "u1", permalink: Some("https://app.pulumi.com") },
        StackEvent::DestroyStarted { update_name: "u1" },
        StackEvent::DestroySucceeded,
        StackEvent::WorkspaceDeleted,
    ];

    for event in &normals {
        assert_eq!(event.severity(), Severity::Normal, "expected Normal for: {}", event.note());
    }
}

#[test]
fn severity_warning_for_failure_events() {
    let warnings: Vec<StackEvent<'_>> = vec![
        StackEvent::UpdateFailed { update_name: "u1", message: "err" },
        StackEvent::LockConflict { update_name: "u1" },
        StackEvent::DestroyFailed { update_name: "u1", attempt: 3 },
        StackEvent::ForceUnlocked,
        StackEvent::Stalled { reason: "SpecInvalid", message: "bad" },
        StackEvent::ProjectNotFound { project_id: "p1" },
        StackEvent::ProjectTtlExpired,
    ];

    for event in &warnings {
        assert_eq!(event.severity(), Severity::Warning, "expected Warning for: {}", event.note());
    }
}

#[test]
fn notification_filter_mapping() {
    // Terminal events map to Some(filter)
    assert_eq!(
        StackEvent::UpdateSucceeded { update_name: "u", permalink: None }.notification_filter(),
        Some(NotificationEventFilter::UpdateSucceeded),
    );
    assert_eq!(
        StackEvent::UpdateFailed { update_name: "u", message: "e" }.notification_filter(),
        Some(NotificationEventFilter::UpdateFailed),
    );
    assert_eq!(
        StackEvent::DestroySucceeded.notification_filter(),
        Some(NotificationEventFilter::DestroySucceeded),
    );
    assert_eq!(
        StackEvent::DestroyFailed { update_name: "u", attempt: 1 }.notification_filter(),
        Some(NotificationEventFilter::DestroyFailed),
    );
    assert_eq!(
        StackEvent::LockConflict { update_name: "u" }.notification_filter(),
        Some(NotificationEventFilter::LockConflict),
    );
    assert_eq!(
        StackEvent::Stalled { reason: "x", message: "y" }.notification_filter(),
        Some(NotificationEventFilter::Stalled),
    );

    // Intermediate events map to None
    assert_eq!(
        StackEvent::UpdateCreated { update_name: "u" }.notification_filter(),
        None,
    );
    assert_eq!(
        StackEvent::DestroyStarted { update_name: "u" }.notification_filter(),
        None,
    );
    assert_eq!(
        StackEvent::WorkspaceDeleted.notification_filter(),
        None,
    );
    assert_eq!(
        StackEvent::ForceUnlocked.notification_filter(),
        None,
    );
    assert_eq!(
        StackEvent::ProjectNotFound { project_id: "p" }.notification_filter(),
        None,
    );
    assert_eq!(
        StackEvent::ProjectTtlExpired.notification_filter(),
        None,
    );
}

#[test]
fn note_contains_variable_data() {
    let event = StackEvent::UpdateCreated { update_name: "my-update-123" };
    assert!(event.note().contains("my-update-123"));

    let event = StackEvent::UpdateFailed {
        update_name: "fail-update",
        message: "something went wrong",
    };
    let note = event.note();
    assert!(note.contains("fail-update"));
    assert!(note.contains("something went wrong"));

    let event = StackEvent::UpdateSucceeded {
        update_name: "ok-update",
        permalink: Some("https://app.pulumi.com/run/123"),
    };
    let note = event.note();
    assert!(note.contains("ok-update"));
    assert!(note.contains("https://app.pulumi.com/run/123"));

    let event = StackEvent::Stalled {
        reason: "SpecInvalid",
        message: "missing field",
    };
    let note = event.note();
    assert!(note.contains("SpecInvalid"));
    assert!(note.contains("missing field"));
}

#[test]
fn notification_event_filter_serde_roundtrip() {
    let variants = vec![
        NotificationEventFilter::UpdateSucceeded,
        NotificationEventFilter::UpdateFailed,
        NotificationEventFilter::DestroySucceeded,
        NotificationEventFilter::DestroyFailed,
        NotificationEventFilter::LockConflict,
        NotificationEventFilter::Stalled,
    ];

    for variant in &variants {
        let json = serde_json::to_string(variant).unwrap();
        let back: NotificationEventFilter = serde_json::from_str(&json).unwrap();
        assert_eq!(*variant, back, "roundtrip failed for {:?}", variant);
    }
}
