use compact_str::CompactString;

/// Authentication decision -- enum-based, no dyn.
pub enum AuthDecision {
    Allow {
        uid: CompactString,
        name: CompactString,
    },
    Deny {
        reason: &'static str,
    },
}

/// Authenticate a bearer token using Kubernetes TokenReview.
/// Then authorize via SubjectAccessReview.
pub async fn authenticate(
    token: &str,
    audiences: &[&str],
    client: &kube::Client,
    workspace_namespace: &str,
    workspace_name: &str,
) -> AuthDecision {
    // Step 1: TokenReview
    let token_review = match create_token_review(client, token, audiences).await {
        Ok(review) => review,
        Err(_) => {
            return AuthDecision::Deny {
                reason: "token review failed",
            }
        }
    };

    let (uid, username) = match extract_identity(&token_review) {
        Some(id) => id,
        None => {
            return AuthDecision::Deny {
                reason: "token not authenticated",
            }
        }
    };

    // Step 2: SubjectAccessReview
    let authorized = check_access(client, &username, workspace_namespace, workspace_name).await;

    if authorized {
        AuthDecision::Allow {
            uid: CompactString::new(&uid),
            name: CompactString::new(&username),
        }
    } else {
        AuthDecision::Deny {
            reason: "not authorized for workspace RPC",
        }
    }
}

async fn create_token_review(
    client: &kube::Client,
    token: &str,
    audiences: &[&str],
) -> Result<k8s_openapi::api::authentication::v1::TokenReview, kube::Error> {
    use k8s_openapi::api::authentication::v1::{TokenReview, TokenReviewSpec};
    use kube::Api;

    let review = TokenReview {
        spec: TokenReviewSpec {
            token: Some(token.to_owned()),
            audiences: Some(audiences.iter().map(|a| a.to_string()).collect()),
        },
        ..Default::default()
    };

    let reviews: Api<TokenReview> = Api::all(client.clone());
    reviews
        .create(&kube::api::PostParams::default(), &review)
        .await
}

fn extract_identity(
    review: &k8s_openapi::api::authentication::v1::TokenReview,
) -> Option<(String, String)> {
    let status = review.status.as_ref()?;
    if !status.authenticated.unwrap_or(false) {
        return None;
    }
    let user = status.user.as_ref()?;
    Some((
        user.uid.clone().unwrap_or_default(),
        user.username.clone().unwrap_or_default(),
    ))
}

async fn check_access(
    client: &kube::Client,
    username: &str,
    namespace: &str,
    workspace_name: &str,
) -> bool {
    use k8s_openapi::api::authorization::v1::{
        ResourceAttributes, SubjectAccessReview, SubjectAccessReviewSpec,
    };
    use kube::Api;

    let review = SubjectAccessReview {
        spec: SubjectAccessReviewSpec {
            user: Some(username.to_owned()),
            resource_attributes: Some(ResourceAttributes {
                namespace: Some(namespace.to_owned()),
                verb: Some("use".to_owned()),
                group: Some("auto.pulumi.com".to_owned()),
                resource: Some("workspaces".to_owned()),
                subresource: Some("rpc".to_owned()),
                name: Some(workspace_name.to_owned()),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let reviews: Api<SubjectAccessReview> = Api::all(client.clone());
    match reviews
        .create(&kube::api::PostParams::default(), &review)
        .await
    {
        Ok(result) => result.status.is_some_and(|s| s.allowed),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::authentication::v1::{TokenReview, TokenReviewStatus, UserInfo};

    fn review_with_status(status: Option<TokenReviewStatus>) -> TokenReview {
        TokenReview {
            status,
            ..Default::default()
        }
    }

    #[test]
    fn extract_identity_no_status() {
        let review = review_with_status(None);
        assert!(extract_identity(&review).is_none());
    }

    #[test]
    fn extract_identity_not_authenticated() {
        let review = review_with_status(Some(TokenReviewStatus {
            authenticated: Some(false),
            ..Default::default()
        }));
        assert!(extract_identity(&review).is_none());
    }

    #[test]
    fn extract_identity_authenticated_none() {
        let review = review_with_status(Some(TokenReviewStatus {
            authenticated: None,
            ..Default::default()
        }));
        assert!(extract_identity(&review).is_none());
    }

    #[test]
    fn extract_identity_valid() {
        let review = review_with_status(Some(TokenReviewStatus {
            authenticated: Some(true),
            user: Some(UserInfo {
                uid: Some("uid-123".into()),
                username: Some("alice".into()),
                ..Default::default()
            }),
            ..Default::default()
        }));
        let (uid, name) = extract_identity(&review).unwrap();
        assert_eq!(uid, "uid-123");
        assert_eq!(name, "alice");
    }

    #[test]
    fn extract_identity_missing_uid_username_defaults() {
        let review = review_with_status(Some(TokenReviewStatus {
            authenticated: Some(true),
            user: Some(UserInfo {
                uid: None,
                username: None,
                ..Default::default()
            }),
            ..Default::default()
        }));
        let (uid, name) = extract_identity(&review).unwrap();
        assert_eq!(uid, "");
        assert_eq!(name, "");
    }

    #[test]
    fn extract_identity_no_user() {
        let review = review_with_status(Some(TokenReviewStatus {
            authenticated: Some(true),
            user: None,
            ..Default::default()
        }));
        assert!(extract_identity(&review).is_none());
    }

    #[test]
    fn auth_decision_allow_construction() {
        let decision = AuthDecision::Allow {
            uid: CompactString::new("u1"),
            name: CompactString::new("bob"),
        };
        match decision {
            AuthDecision::Allow { uid, name } => {
                assert_eq!(uid.as_str(), "u1");
                assert_eq!(name.as_str(), "bob");
            }
            AuthDecision::Deny { .. } => panic!("expected Allow"),
        }
    }

    #[test]
    fn auth_decision_deny_construction() {
        let decision = AuthDecision::Deny {
            reason: "forbidden",
        };
        match decision {
            AuthDecision::Deny { reason } => assert_eq!(reason, "forbidden"),
            AuthDecision::Allow { .. } => panic!("expected Deny"),
        }
    }
}
