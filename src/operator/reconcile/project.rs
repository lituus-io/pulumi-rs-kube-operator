use std::time::Duration;

use crate::api::stack::{ProjectCheckStatus, ProjectVerification, Stack};
use crate::core::time::elapsed_since;
use crate::operator::manager::Manager;

/// Result of a project verification check.
#[derive(Debug, PartialEq)]
pub enum ProjectCheckResult {
    /// No projectVerification configured on the stack.
    NotConfigured,
    /// Project exists and is active.
    Active,
    /// Project not found in the cloud provider.
    NotFound { project_id: String },
    /// Error while checking (treated as transient — do not start grace period).
    Error { message: String },
}

/// Check whether the cloud project referenced by this Stack still exists.
/// Reads the project ID from the Program's variables via the reflector store (zero API calls).
/// Then checks GCP Cloud Resource Manager v3 API.
pub async fn check_project(mgr: &Manager, ns: &str, stack: &Stack) -> ProjectCheckResult {
    let verification = match &stack.spec.project_verification {
        Some(v) => v,
        None => return ProjectCheckResult::NotConfigured,
    };

    // Rate-limit: don't recheck more often than every 5 minutes
    if let Some(status) = stack
        .status
        .as_ref()
        .and_then(|s| s.last_project_check.as_ref())
    {
        if elapsed_since(Some(status.checked_at.as_str())) < Duration::from_secs(300) {
            // Return the cached result
            return match status.result.as_str() {
                "active" => ProjectCheckResult::Active,
                "not_found" => ProjectCheckResult::NotFound {
                    project_id: "cached".to_owned(),
                },
                _ => ProjectCheckResult::Active, // Optimistic for cached errors
            };
        }
    }

    // Extract project ID from Program variables
    let project_id = match extract_project_id(mgr, ns, stack, &verification.variable_name) {
        Some(id) => id,
        None => {
            tracing::debug!(
                variable = %verification.variable_name,
                "project verification variable not found in program, skipping"
            );
            return ProjectCheckResult::NotConfigured;
        }
    };

    // Check project existence via GCP API
    match &*verification.provider {
        "gcp" => check_gcp_project(&project_id, verification).await,
        other => {
            tracing::warn!(provider = %other, "unsupported project verification provider");
            ProjectCheckResult::Error {
                message: format!("unsupported provider: {}", other),
            }
        }
    }
}

/// Extract the project ID from the Program's variables using the reflector store.
fn extract_project_id(
    mgr: &Manager,
    ns: &str,
    stack: &Stack,
    variable_name: &str,
) -> Option<String> {
    let program_ref = stack.spec.program_ref.as_ref()?;
    let prog_key = kube::runtime::reflector::ObjectRef::new(&program_ref.name).within(ns);
    let program = mgr.stores.programs.get(&prog_key)?;

    program
        .spec
        .variables
        .as_ref()?
        .get(variable_name)
        .and_then(|v| match v {
            serde_json::Value::String(s) => Some(s.clone()),
            _ => v.as_str().map(|s| s.to_owned()),
        })
}

/// Check if a GCP project exists using Cloud Resource Manager v3.
/// Uses ADC (Application Default Credentials) from the metadata server or
/// a credential secret if specified.
async fn check_gcp_project(
    project_id: &str,
    _verification: &ProjectVerification,
) -> ProjectCheckResult {
    // GCP Cloud Resource Manager v3: GET /v3/projects/{project_id}
    let url = format!(
        "https://cloudresourcemanager.googleapis.com/v3/projects/{}",
        project_id
    );

    // Get access token from GKE metadata server (workload identity / ADC)
    let token = match get_gcp_access_token().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "failed to get GCP access token for project check");
            return ProjectCheckResult::Error {
                message: format!("auth: {}", e),
            };
        }
    };

    let client = reqwest::Client::new();
    match client
        .get(&url)
        .header("Authorization", format!("Bearer {}", token))
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            match status {
                200 => {
                    // Check lifecycleState in response
                    if let Ok(body) = resp.json::<serde_json::Value>().await {
                        let state = body
                            .get("state")
                            .and_then(|s| s.as_str())
                            .unwrap_or("ACTIVE");
                        if state == "ACTIVE" {
                            ProjectCheckResult::Active
                        } else {
                            tracing::info!(
                                project = %project_id,
                                state,
                                "project exists but is not active"
                            );
                            ProjectCheckResult::NotFound {
                                project_id: project_id.to_owned(),
                            }
                        }
                    } else {
                        ProjectCheckResult::Active
                    }
                }
                403 => {
                    // Permission denied — project may exist but we can't see it.
                    // Treat as error (don't start grace period).
                    ProjectCheckResult::Error {
                        message: format!("permission denied for project {}", project_id),
                    }
                }
                404 => ProjectCheckResult::NotFound {
                    project_id: project_id.to_owned(),
                },
                _ => ProjectCheckResult::Error {
                    message: format!("GCP API returned {}", status),
                },
            }
        }
        Err(e) => ProjectCheckResult::Error {
            message: format!("GCP API request failed: {}", e),
        },
    }
}

/// Get an access token from the GKE metadata server (workload identity).
async fn get_gcp_access_token() -> Result<String, String> {
    let client = reqwest::Client::new();
    let resp = client
        .get("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token")
        .header("Metadata-Flavor", "Google")
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| format!("metadata request failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("metadata server returned {}", resp.status()));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse token response: {}", e))?;

    body.get("access_token")
        .and_then(|t| t.as_str())
        .map(|s| s.to_owned())
        .ok_or_else(|| "no access_token in metadata response".to_owned())
}

/// Check if the grace period has expired.
pub fn is_grace_period_expired(pending_since: &str, grace_period_days: i64) -> bool {
    let grace = Duration::from_secs((grace_period_days.max(0) as u64) * 86400);
    elapsed_since(Some(pending_since)) >= grace
}

/// Build a ProjectCheckStatus for status patching.
pub fn build_check_status(result: &ProjectCheckResult) -> ProjectCheckStatus {
    let now = chrono::Utc::now().to_rfc3339();
    match result {
        ProjectCheckResult::NotConfigured => ProjectCheckStatus {
            checked_at: now,
            result: "not_configured".to_owned(),
            message: None,
        },
        ProjectCheckResult::Active => ProjectCheckStatus {
            checked_at: now,
            result: "active".to_owned(),
            message: None,
        },
        ProjectCheckResult::NotFound { project_id } => ProjectCheckStatus {
            checked_at: now,
            result: "not_found".to_owned(),
            message: Some(format!("project {} not found", project_id)),
        },
        ProjectCheckResult::Error { message } => ProjectCheckStatus {
            checked_at: now,
            result: "error".to_owned(),
            message: Some(message.clone()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grace_period_not_expired_recent() {
        let now = chrono::Utc::now().to_rfc3339();
        assert!(!is_grace_period_expired(&now, 30));
    }

    #[test]
    fn grace_period_expired_old() {
        let old = (chrono::Utc::now() - chrono::Duration::days(31)).to_rfc3339();
        assert!(is_grace_period_expired(&old, 30));
    }

    #[test]
    fn grace_period_zero_days_always_expired() {
        let now = chrono::Utc::now().to_rfc3339();
        // 0 grace days = expire immediately
        assert!(is_grace_period_expired(&now, 0));
    }

    #[test]
    fn build_check_status_active() {
        let status = build_check_status(&ProjectCheckResult::Active);
        assert_eq!(status.result, "active");
        assert!(status.message.is_none());
    }

    #[test]
    fn build_check_status_not_found() {
        let status = build_check_status(&ProjectCheckResult::NotFound {
            project_id: "test-123".to_owned(),
        });
        assert_eq!(status.result, "not_found");
        assert!(status.message.unwrap().contains("test-123"));
    }

    #[test]
    fn build_check_status_error() {
        let status = build_check_status(&ProjectCheckResult::Error {
            message: "auth failed".to_owned(),
        });
        assert_eq!(status.result, "error");
        assert_eq!(status.message.unwrap(), "auth failed");
    }
}
