use compact_str::CompactString;
use kube::api::DynamicObject;
use kube::discovery::ApiResource;
use kube::runtime::reflector::Store;
use kube::Api;

use crate::api::program::Program;
use crate::api::stack::{FluxSource, ProgramReference, StackSpec};
use crate::errors::{OperatorError, PermanentError, TransientError};
use crate::operator::manager::Manager;

/// Resolved source information.
pub struct SourceInfo {
    pub commit: CompactString,
    pub source_type: SourceType,
}

#[derive(Debug)]
pub enum SourceType {
    Git,
    Flux,
    Program,
}

/// No dyn Trait. Each variant carries borrowed references to spec fields.
pub enum SourceKind<'spec> {
    Git {
        repo: &'spec str,
        branch: Option<&'spec str>,
        commit: Option<&'spec str>,
    },
    Flux {
        source: &'spec FluxSource,
    },
    Program {
        program_ref: &'spec ProgramReference,
    },
}

impl<'spec> SourceKind<'spec> {
    /// Extract from spec without allocation.
    pub fn from_spec(spec: &'spec StackSpec) -> Result<Self, OperatorError> {
        match (
            spec.project_repo.as_deref(),
            spec.flux_source.as_ref(),
            spec.program_ref.as_ref(),
        ) {
            (Some(repo), None, None) => Ok(Self::Git {
                repo,
                branch: spec.branch.as_deref(),
                commit: spec.commit.as_deref(),
            }),
            (None, Some(flux), None) => Ok(Self::Flux { source: flux }),
            (None, None, Some(prog)) => Ok(Self::Program { program_ref: prog }),
            (None, None, None) => Err(OperatorError::Permanent(PermanentError::SpecInvalid {
                field: "source",
            })),
            _ => Err(OperatorError::Permanent(PermanentError::SpecInvalid {
                field: "source",
            })),
        }
    }

    /// Resolve the source to get commit/revision info.
    pub async fn resolve(&self, mgr: &Manager, ns: &str) -> Result<SourceInfo, OperatorError> {
        match self {
            Self::Git {
                repo: _,
                branch: _,
                commit,
            } => resolve_git(*commit),
            Self::Flux { source } => resolve_flux(&mgr.client, ns, source).await,
            Self::Program { program_ref } => resolve_program(&mgr.stores.programs, ns, program_ref),
        }
    }
}

fn resolve_git(commit: Option<&str>) -> Result<SourceInfo, OperatorError> {
    // For git sources, the commit is determined by the agent during workspace init.
    // Here we use the specified commit or a placeholder that will be resolved later.
    let commit = match commit {
        Some(c) => CompactString::new(c),
        None => CompactString::new("HEAD"),
    };

    Ok(SourceInfo {
        commit,
        source_type: SourceType::Git,
    })
}

/// Resolve a Flux source using DynamicObject.
/// Reads `status.artifact.revision` and `status.artifact.url` from the Flux source CR.
/// Supports GitRepository, Bucket, and OCIRepository.
async fn resolve_flux(
    client: &kube::Client,
    ns: &str,
    source: &FluxSource,
) -> Result<SourceInfo, OperatorError> {
    let source_ref = &source.source_ref;

    // Parse apiVersion into group/version
    let (group, version) = parse_api_version(&source_ref.api_version)?;

    let ar = ApiResource {
        group: group.to_owned(),
        version: version.to_owned(),
        api_version: source_ref.api_version.clone(),
        kind: source_ref.kind.clone(),
        plural: pluralize_kind(&source_ref.kind),
    };

    let dynapi: Api<DynamicObject> = Api::namespaced_with(client.clone(), ns, &ar);

    let obj = dynapi.get(&source_ref.name).await.map_err(|e| match e {
        kube::Error::Api(ref api_err) if api_err.code == 404 => {
            OperatorError::Permanent(PermanentError::SourceUnavailable)
        }
        _ => OperatorError::Transient(TransientError::KubeApi {
            reason: "failed to get Flux source",
        }),
    })?;

    // Extract status.artifact.revision from the dynamic object
    let revision = obj
        .data
        .get("status")
        .and_then(|s| s.get("artifact"))
        .and_then(|a| a.get("revision"))
        .and_then(|r| r.as_str())
        .ok_or(OperatorError::Transient(TransientError::ArtifactNotReady))?;

    tracing::debug!(
        kind = %source_ref.kind,
        name = %source_ref.name,
        revision = %revision,
        "resolved Flux source"
    );

    Ok(SourceInfo {
        commit: CompactString::new(revision),
        source_type: SourceType::Flux,
    })
}

fn resolve_program(
    store: &Store<Program>,
    ns: &str,
    program_ref: &ProgramReference,
) -> Result<SourceInfo, OperatorError> {
    // Use the shared program store instead of Api::get()
    let key = kube::runtime::reflector::ObjectRef::new(&program_ref.name).within(ns);
    let program = store
        .get(&key)
        .ok_or(OperatorError::Permanent(PermanentError::ProgramNotFound))?;

    // Use the artifact revision as the commit hash
    let commit = program
        .status
        .as_ref()
        .and_then(|s| s.artifact.as_ref())
        .map(|a| CompactString::new(&a.revision))
        .ok_or(OperatorError::Transient(TransientError::ArtifactNotReady))?;

    Ok(SourceInfo {
        commit,
        source_type: SourceType::Program,
    })
}

/// Parse "source.toolkit.fluxcd.io/v1" → ("source.toolkit.fluxcd.io", "v1")
fn parse_api_version(api_version: &str) -> Result<(&str, &str), OperatorError> {
    match api_version.rsplit_once('/') {
        Some((group, version)) => Ok((group, version)),
        None => Err(OperatorError::Permanent(PermanentError::SpecInvalid {
            field: "sourceRef.apiVersion",
        })),
    }
}

/// Convert Kind to plural resource name (lowercase + "s").
/// Matches Kubernetes convention for standard Flux source types.
fn pluralize_kind(kind: &str) -> String {
    match kind {
        "GitRepository" => "gitrepositories".to_owned(),
        "Bucket" => "buckets".to_owned(),
        "OCIRepository" => "ocirepositories".to_owned(),
        _ => {
            let lower = kind.to_lowercase();
            if lower.ends_with('s') {
                lower
            } else {
                format!("{}s", lower)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_api_version() {
        let (g, v) = parse_api_version("source.toolkit.fluxcd.io/v1").unwrap();
        assert_eq!(g, "source.toolkit.fluxcd.io");
        assert_eq!(v, "v1");
    }

    #[test]
    fn test_parse_api_version_v1beta2() {
        let (g, v) = parse_api_version("source.toolkit.fluxcd.io/v1beta2").unwrap();
        assert_eq!(g, "source.toolkit.fluxcd.io");
        assert_eq!(v, "v1beta2");
    }

    #[test]
    fn test_parse_api_version_invalid() {
        assert!(parse_api_version("v1").is_err());
    }

    #[test]
    fn test_pluralize_kind() {
        assert_eq!(pluralize_kind("GitRepository"), "gitrepositories");
        assert_eq!(pluralize_kind("Bucket"), "buckets");
        assert_eq!(pluralize_kind("OCIRepository"), "ocirepositories");
        assert_eq!(pluralize_kind("CustomThing"), "customthings");
    }
}
