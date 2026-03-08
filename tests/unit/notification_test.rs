use pulumi_kubernetes_operator::api::stack::{
    NotificationEventFilter, NotificationWebhook, StackSpec, WebhookAuthSecret,
};

#[test]
fn webhook_config_deserializes_minimal() {
    let json = r#"{ "url": "https://hooks.slack.com/services/x" }"#;
    let webhook: NotificationWebhook = serde_json::from_str(json).unwrap();
    assert_eq!(webhook.url, "https://hooks.slack.com/services/x");
    assert!(webhook.auth_secret.is_none());
    assert!(webhook.events.is_empty());
}

#[test]
fn webhook_config_deserializes_full() {
    let json = r#"{
        "url": "https://hooks.slack.com/services/x",
        "authSecret": { "name": "webhook-token", "key": "token" },
        "events": ["UpdateSucceeded", "UpdateFailed", "Stalled"]
    }"#;
    let webhook: NotificationWebhook = serde_json::from_str(json).unwrap();
    assert_eq!(webhook.url, "https://hooks.slack.com/services/x");
    let auth = webhook.auth_secret.unwrap();
    assert_eq!(auth.name, "webhook-token");
    assert_eq!(auth.key, "token");
    assert_eq!(webhook.events.len(), 3);
    assert_eq!(webhook.events[0], NotificationEventFilter::UpdateSucceeded);
    assert_eq!(webhook.events[1], NotificationEventFilter::UpdateFailed);
    assert_eq!(webhook.events[2], NotificationEventFilter::Stalled);
}

#[test]
fn stack_spec_with_notifications_deserializes() {
    let json = r#"{
        "stack": "org/my-stack",
        "notifications": [
            { "url": "https://hook1.example.com" },
            {
                "url": "https://hook2.example.com",
                "events": ["DestroySucceeded"]
            }
        ]
    }"#;
    let spec: StackSpec = serde_json::from_str(json).unwrap();
    assert_eq!(spec.notifications.len(), 2);
    assert_eq!(spec.notifications[0].url, "https://hook1.example.com");
    assert!(spec.notifications[0].events.is_empty());
    assert_eq!(spec.notifications[1].events[0], NotificationEventFilter::DestroySucceeded);
}

#[test]
fn stack_spec_without_notifications_deserializes() {
    let json = r#"{ "stack": "org/my-stack" }"#;
    let spec: StackSpec = serde_json::from_str(json).unwrap();
    assert!(spec.notifications.is_empty());
}

#[test]
fn webhook_auth_secret_deserializes() {
    let json = r#"{ "name": "my-secret", "key": "api-key" }"#;
    let auth: WebhookAuthSecret = serde_json::from_str(json).unwrap();
    assert_eq!(auth.name, "my-secret");
    assert_eq!(auth.key, "api-key");
}

#[test]
fn notification_event_filter_all_variants_serializable() {
    let variants = vec![
        (NotificationEventFilter::UpdateSucceeded, "\"UpdateSucceeded\""),
        (NotificationEventFilter::UpdateFailed, "\"UpdateFailed\""),
        (NotificationEventFilter::DestroySucceeded, "\"DestroySucceeded\""),
        (NotificationEventFilter::DestroyFailed, "\"DestroyFailed\""),
        (NotificationEventFilter::LockConflict, "\"LockConflict\""),
        (NotificationEventFilter::Stalled, "\"Stalled\""),
    ];

    for (variant, expected_json) in &variants {
        let serialized = serde_json::to_string(variant).unwrap();
        assert_eq!(&serialized, *expected_json);
        let deserialized: NotificationEventFilter = serde_json::from_str(&serialized).unwrap();
        assert_eq!(*variant, deserialized);
    }
}
