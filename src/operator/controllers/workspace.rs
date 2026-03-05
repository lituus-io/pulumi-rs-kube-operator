use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::{StatefulSet, StatefulSetSpec};
use k8s_openapi::api::core::v1::{
    Container, ContainerPort, EmptyDirVolumeSource, EnvVar, EnvVarSource, PodSpec,
    PodTemplateSpec, SecretKeySelector, SecurityContext, Service, ServicePort, ServiceSpec, Volume,
    VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::Api;
use sha2::{Digest, Sha256};

use crate::api::conditions::{
    AUTO_COMPONENT_LABEL, POD_REVISION_HASH_ANN, WORKSPACE_NAME_LABEL,
};
use crate::api::stack::{ResourceSelectorType, Stack};
use crate::api::workspace::{SecurityProfile, Workspace, WorkspaceSpec};
use crate::errors::{OperatorError, TransientError};
use crate::operator::manager::Manager;

const GRPC_PORT: i32 = 50051;
const TERMINATION_GRACE_PERIOD: i64 = 600;

/// Read the agent/workspace image from the AGENT_IMAGE env var (set by Helm chart),
/// falling back to a dev default.
pub fn agent_image() -> String {
    std::env::var("AGENT_IMAGE").unwrap_or_else(|_| "pulumi-kubernetes-operator:dev".to_owned())
}

/// Build the desired Workspace spec from a Stack.
pub fn build_workspace_spec(stack: &Stack) -> WorkspaceSpec {
    let spec = &stack.spec;

    let git = spec.project_repo.as_ref().map(|repo| {
        crate::api::workspace::WorkspaceGitSource {
            url: Some(repo.clone()),
            git_ref: spec
                .branch
                .as_ref()
                .map(|b| b.to_string())
                .or_else(|| spec.commit.as_ref().map(|c| c.to_string())),
            dir: spec.repo_dir.clone(),
            auth: None,
            shallow: spec.shallow,
        }
    });

    let flux = spec.flux_source.as_ref().map(|fs| {
        crate::api::workspace::WorkspaceFluxSource {
            url: None,
            digest: None,
            dir: fs.dir.clone(),
        }
    });

    let stacks = vec![crate::api::workspace::WorkspaceStack {
        name: spec.stack.to_string(),
        create: Some(!spec.use_local_stack_only),
        secrets_provider: spec.secrets_provider.clone(),
        config: vec![],
        environment: spec.environment.clone(),
    }];

    WorkspaceSpec {
        service_account_name: spec.service_account_name.clone(),
        security_profile: SecurityProfile::Restricted,
        image: None,
        image_pull_policy: None,
        git,
        flux,
        local: None,
        env_from: vec![],
        env: vec![],
        resources: None,
        pod_template: None,
        pulumi_log_verbosity: 0,
        stacks,
    }
}

/// Build env vars from Stack envRefs and backend for workspace containers.
pub fn build_env_vars(stack: &Stack) -> Vec<EnvVar> {
    let mut envs: Vec<EnvVar> = Vec::new();

    // Propagate backend as PULUMI_BACKEND_URL
    if let Some(ref backend) = stack.spec.backend {
        envs.push(EnvVar {
            name: "PULUMI_BACKEND_URL".to_owned(),
            value: Some(backend.clone()),
            ..Default::default()
        });
    }

    // Propagate envRefs
    if let Some(ref env_refs) = stack.spec.env_refs {
        for (env_name, resource_ref) in env_refs {
            match resource_ref.selector_type {
                ResourceSelectorType::Secret => {
                    if let Some(ref secret) = resource_ref.secret_ref {
                        envs.push(EnvVar {
                            name: env_name.clone(),
                            value_from: Some(EnvVarSource {
                                secret_key_ref: Some(SecretKeySelector {
                                    name: secret.name.clone(),
                                    key: secret.key.clone(),
                                    optional: Some(false),
                                }),
                                ..Default::default()
                            }),
                            ..Default::default()
                        });
                    }
                }
                ResourceSelectorType::Literal => {
                    if let Some(ref literal) = resource_ref.literal_ref {
                        envs.push(EnvVar {
                            name: env_name.clone(),
                            value: Some(literal.value.clone()),
                            ..Default::default()
                        });
                    }
                }
                ResourceSelectorType::Env => {
                    if let Some(ref env_sel) = resource_ref.env {
                        envs.push(EnvVar {
                            name: env_name.clone(),
                            value: std::env::var(&env_sel.name).ok(),
                            ..Default::default()
                        });
                    }
                }
                ResourceSelectorType::FS => {
                    // FS-type refs are mounted as files, not env vars - skip
                }
            }
        }
    }

    envs
}

/// Build the StatefulSet for a workspace pod.
/// Matches the Go operator's exact spec: bootstrap + fetch init containers, pulumi main container.
pub fn build_statefulset(
    ws: &Workspace,
    ws_name: &str,
    ns: &str,
    owner_ref: OwnerReference,
    image: &str,
    extra_env: Vec<EnvVar>,
    program_url: Option<&str>,
) -> StatefulSet {
    let sts_name = format!("{}-workspace", ws_name);
    let revision_hash = compute_revision_hash(ws);

    let labels: BTreeMap<String, String> = [
        (AUTO_COMPONENT_LABEL.to_owned(), "workspace".to_owned()),
        (WORKSPACE_NAME_LABEL.to_owned(), ws_name.to_owned()),
    ]
    .into();

    let mut annotations: BTreeMap<String, String> = BTreeMap::new();
    annotations.insert(POD_REVISION_HASH_ANN.to_owned(), revision_hash);

    // Init container 1: bootstrap — copy operator binary and tini to shared volume
    let bootstrap = Container {
        name: "bootstrap".to_owned(),
        image: Some(image.to_owned()),
        command: Some(vec!["sh".to_owned(), "-c".to_owned()]),
        args: Some(vec![
            "cp /usr/local/bin/pulumi-kubernetes-operator /share/agent && cp /usr/bin/tini /share/tini".to_owned(),
        ]),
        volume_mounts: Some(vec![share_mount()]),
        ..Default::default()
    };

    // Init container 2: fetch — use env vars (not CLI args) to match init.rs
    let mut fetch_env: Vec<EnvVar> = Vec::new();
    if let Some(ref git) = ws.spec.git {
        if let Some(ref url) = git.url {
            fetch_env.push(EnvVar {
                name: "GIT_URL".to_owned(),
                value: Some(url.clone()),
                ..Default::default()
            });
        }
        if let Some(ref git_ref) = git.git_ref {
            fetch_env.push(EnvVar {
                name: "GIT_REVISION".to_owned(),
                value: Some(git_ref.clone()),
                ..Default::default()
            });
        }
        if let Some(ref dir) = git.dir {
            fetch_env.push(EnvVar {
                name: "GIT_DIR".to_owned(),
                value: Some(dir.clone()),
                ..Default::default()
            });
        }
    }
    if let Some(ref flux) = ws.spec.flux {
        if let Some(ref url) = flux.url {
            fetch_env.push(EnvVar {
                name: "FLUX_URL".to_owned(),
                value: Some(url.clone()),
                ..Default::default()
            });
        }
        if let Some(ref digest) = flux.digest {
            fetch_env.push(EnvVar {
                name: "FLUX_DIGEST".to_owned(),
                value: Some(digest.clone()),
                ..Default::default()
            });
        }
        if let Some(ref dir) = flux.dir {
            fetch_env.push(EnvVar {
                name: "FLUX_DIR".to_owned(),
                value: Some(dir.clone()),
                ..Default::default()
            });
        }
    }
    if let Some(url) = program_url {
        fetch_env.push(EnvVar {
            name: "PROGRAM_URL".to_owned(),
            value: Some(url.to_owned()),
            ..Default::default()
        });
    }

    let fetch = Container {
        name: "fetch".to_owned(),
        image: Some(image.to_owned()),
        command: Some(vec![
            "/usr/local/bin/pulumi-kubernetes-operator".to_owned(),
        ]),
        args: Some(vec!["init".to_owned()]),
        env: if fetch_env.is_empty() {
            None
        } else {
            Some(fetch_env)
        },
        volume_mounts: Some(vec![share_mount()]),
        ..Default::default()
    };

    // Main container: agent gRPC server
    let main_args = vec![
        "agent".to_owned(),
        "--listen-address".to_owned(),
        "0.0.0.0:50051".to_owned(),
        "--workspace-dir".to_owned(),
        "/share/workspace".to_owned(),
    ];

    let security_ctx = match ws.spec.security_profile {
        SecurityProfile::Restricted => Some(SecurityContext {
            run_as_non_root: Some(true),
            run_as_user: Some(1000),
            allow_privilege_escalation: Some(false),
            capabilities: Some(k8s_openapi::api::core::v1::Capabilities {
                drop: Some(vec!["ALL".to_owned()]),
                ..Default::default()
            }),
            seccomp_profile: Some(k8s_openapi::api::core::v1::SeccompProfile {
                type_: "RuntimeDefault".to_owned(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        SecurityProfile::Baseline => None,
    };

    let pulumi_container = Container {
        name: "pulumi".to_owned(),
        image: Some(image.to_owned()),
        command: Some(vec![
            "/share/tini".to_owned(),
            "--".to_owned(),
            "/usr/local/bin/pulumi-kubernetes-operator".to_owned(),
        ]),
        args: Some(main_args),
        ports: Some(vec![ContainerPort {
            container_port: GRPC_PORT,
            name: Some("grpc".to_owned()),
            ..Default::default()
        }]),
        env: if extra_env.is_empty() {
            None
        } else {
            Some(extra_env)
        },
        volume_mounts: Some(vec![share_mount()]),
        security_context: security_ctx,
        ..Default::default()
    };

    let share_vol = Volume {
        name: "share".to_owned(),
        empty_dir: Some(EmptyDirVolumeSource::default()),
        ..Default::default()
    };

    let pod_spec = PodSpec {
        init_containers: Some(vec![bootstrap, fetch]),
        containers: vec![pulumi_container],
        volumes: Some(vec![share_vol]),
        service_account_name: ws.spec.service_account_name.clone(),
        termination_grace_period_seconds: Some(TERMINATION_GRACE_PERIOD),
        ..Default::default()
    };

    StatefulSet {
        metadata: ObjectMeta {
            name: Some(sts_name.clone()),
            namespace: Some(ns.to_owned()),
            labels: Some(labels.clone()),
            owner_references: Some(vec![owner_ref]),
            ..Default::default()
        },
        spec: Some(StatefulSetSpec {
            replicas: Some(1),
            pod_management_policy: Some("Parallel".to_owned()),
            service_name: sts_name,
            selector: LabelSelector {
                match_labels: Some(labels.clone()),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels),
                    annotations: Some(annotations),
                    ..Default::default()
                }),
                spec: Some(pod_spec),
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Build the headless service for the workspace.
pub fn build_headless_service(
    ws_name: &str,
    ns: &str,
    owner_ref: OwnerReference,
) -> Service {
    let svc_name = format!("{}-workspace", ws_name);
    let labels: BTreeMap<String, String> = [
        (AUTO_COMPONENT_LABEL.to_owned(), "workspace".to_owned()),
        (WORKSPACE_NAME_LABEL.to_owned(), ws_name.to_owned()),
    ]
    .into();

    Service {
        metadata: ObjectMeta {
            name: Some(svc_name),
            namespace: Some(ns.to_owned()),
            labels: Some(labels.clone()),
            owner_references: Some(vec![owner_ref]),
            ..Default::default()
        },
        spec: Some(ServiceSpec {
            cluster_ip: Some("None".to_owned()),
            selector: Some(labels),
            ports: Some(vec![ServicePort {
                port: GRPC_PORT,
                target_port: Some(IntOrString::Int(GRPC_PORT)),
                name: Some("grpc".to_owned()),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Check if workspace is ready:
/// - sts.status.observedGeneration == sts.generation
/// - sts.status.updateRevision == sts.status.currentRevision
/// - sts.status.availableReplicas >= 1
pub fn is_statefulset_ready(sts: &StatefulSet) -> bool {
    let meta_gen = sts.metadata.generation.unwrap_or(0);
    let status = match sts.status.as_ref() {
        Some(s) => s,
        None => return false,
    };

    let observed = status.observed_generation.unwrap_or(0);
    if observed != meta_gen {
        return false;
    }

    let update_rev = status.update_revision.as_deref().unwrap_or("");
    let current_rev = status.current_revision.as_deref().unwrap_or("");
    if update_rev != current_rev {
        return false;
    }

    status.available_replicas.unwrap_or(0) >= 1
}

/// Check if a workspace pod is ready and connectable.
pub async fn is_workspace_ready(
    mgr: &Manager,
    ns: &str,
    name: &str,
) -> Result<bool, OperatorError> {
    let workspaces: Api<Workspace> = Api::namespaced(mgr.client.clone(), ns);

    match workspaces.get(name).await {
        Ok(ws) => {
            let ready = ws
                .status
                .as_ref()
                .map(|s| {
                    s.conditions
                        .iter()
                        .any(|c| c.type_ == "Ready" && c.status == "True")
                })
                .unwrap_or(false);

            Ok(ready)
        }
        Err(kube::Error::Api(err)) if err.code == 404 => Ok(false),
        Err(_) => Err(OperatorError::Transient(TransientError::WorkspaceNotReady)),
    }
}

/// Get the workspace address (service endpoint).
pub fn get_workspace_address(ws_name: &str, ns: &str) -> String {
    // Check WORKSPACE_LOCALHOST for local development
    if let Ok(localhost) = std::env::var("WORKSPACE_LOCALHOST") {
        return localhost;
    }
    format!("{}-workspace.{}:50051", ws_name, ns)
}

/// Compute a revision hash for the workspace spec to detect changes.
fn compute_revision_hash(ws: &Workspace) -> String {
    let mut hasher = Sha256::new();
    if let Ok(json) = serde_json::to_string(&ws.spec) {
        hasher.update(json.as_bytes());
    }
    hex::encode(&hasher.finalize()[..8])
}

fn share_mount() -> VolumeMount {
    VolumeMount {
        name: "share".to_owned(),
        mount_path: "/share".to_owned(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::workspace::{WorkspaceGitSource, WorkspaceSpec};

    const TEST_IMAGE: &str = "pulumi-kubernetes-operator:test";

    fn make_workspace(name: &str, git_url: Option<&str>) -> Workspace {
        let git = git_url.map(|url| WorkspaceGitSource {
            url: Some(url.to_owned()),
            git_ref: Some("main".to_owned()),
            dir: Some("infra".to_owned()),
            auth: None,
            shallow: false,
        });

        Workspace::new(name, WorkspaceSpec {
            service_account_name: Some("pulumi".to_owned()),
            security_profile: SecurityProfile::Restricted,
            image: None,
            image_pull_policy: None,
            git,
            flux: None,
            local: None,
            env_from: vec![],
            env: vec![],
            resources: None,
            pod_template: None,
            pulumi_log_verbosity: 0,
            stacks: vec![],
        })
    }

    fn test_owner_ref() -> OwnerReference {
        OwnerReference {
            api_version: "auto.pulumi.com/v1alpha1".to_owned(),
            kind: "Workspace".to_owned(),
            name: "test-ws".to_owned(),
            uid: "uid-1234".to_owned(),
            controller: Some(true),
            block_owner_deletion: Some(true),
        }
    }

    #[test]
    fn test_build_statefulset_restricted() {
        let ws = make_workspace("test-ws", Some("https://github.com/example/repo"));
        let sts = build_statefulset(&ws, "test-ws", "default", test_owner_ref(), TEST_IMAGE, vec![], None);

        let spec = sts.spec.as_ref().unwrap();
        assert_eq!(spec.replicas, Some(1));
        assert_eq!(spec.pod_management_policy.as_deref(), Some("Parallel"));

        let pod_spec = spec.template.spec.as_ref().unwrap();
        assert_eq!(pod_spec.termination_grace_period_seconds, Some(600));

        // Check init containers
        let init_containers = pod_spec.init_containers.as_ref().unwrap();
        assert_eq!(init_containers.len(), 2);
        assert_eq!(init_containers[0].name, "bootstrap");
        assert_eq!(init_containers[1].name, "fetch");

        // Check main container
        assert_eq!(pod_spec.containers.len(), 1);
        assert_eq!(pod_spec.containers[0].name, "pulumi");
        let ports = pod_spec.containers[0].ports.as_ref().unwrap();
        assert_eq!(ports[0].container_port, 50051);

        // Check security context (restricted profile)
        let sec = pod_spec.containers[0].security_context.as_ref().unwrap();
        assert_eq!(sec.run_as_user, Some(1000));
        assert_eq!(sec.run_as_non_root, Some(true));
        assert_eq!(sec.allow_privilege_escalation, Some(false));
    }

    #[test]
    fn test_build_statefulset_baseline() {
        let mut ws = make_workspace("test-ws", Some("https://github.com/example/repo"));
        ws.spec.security_profile = SecurityProfile::Baseline;
        let sts = build_statefulset(&ws, "test-ws", "default", test_owner_ref(), TEST_IMAGE, vec![], None);

        let pod_spec = sts.spec.as_ref().unwrap().template.spec.as_ref().unwrap();
        // Baseline: no security context on main container
        assert!(pod_spec.containers[0].security_context.is_none());
    }

    #[test]
    fn test_build_service_headless() {
        let svc = build_headless_service("test-ws", "default", test_owner_ref());
        let spec = svc.spec.as_ref().unwrap();

        assert_eq!(spec.cluster_ip.as_deref(), Some("None"));
        let ports = spec.ports.as_ref().unwrap();
        assert_eq!(ports[0].port, 50051);
        assert_eq!(ports[0].name.as_deref(), Some("grpc"));
    }

    #[test]
    fn test_git_source_init_container_env_vars() {
        let ws = make_workspace("test-ws", Some("https://github.com/example/repo"));
        let sts = build_statefulset(&ws, "test-ws", "default", test_owner_ref(), TEST_IMAGE, vec![], None);

        let init_containers = sts.spec.as_ref().unwrap()
            .template.spec.as_ref().unwrap()
            .init_containers.as_ref().unwrap();
        let fetch = &init_containers[1];

        // Fetch container should use env vars, not CLI args
        let env = fetch.env.as_ref().expect("fetch container should have env vars");
        let env_map: BTreeMap<&str, &str> = env.iter()
            .map(|e| (e.name.as_str(), e.value.as_deref().unwrap_or("")))
            .collect();
        assert_eq!(env_map.get("GIT_URL"), Some(&"https://github.com/example/repo"));
        assert_eq!(env_map.get("GIT_REVISION"), Some(&"main"));
        assert_eq!(env_map.get("GIT_DIR"), Some(&"infra"));

        // Args should just be ["init"]
        let args = fetch.args.as_ref().unwrap();
        assert_eq!(args, &["init"]);
    }

    #[test]
    fn test_bootstrap_copies_from_correct_paths() {
        let ws = make_workspace("test-ws", None);
        let sts = build_statefulset(&ws, "test-ws", "default", test_owner_ref(), TEST_IMAGE, vec![], None);

        let init_containers = sts.spec.as_ref().unwrap()
            .template.spec.as_ref().unwrap()
            .init_containers.as_ref().unwrap();
        let bootstrap = &init_containers[0];

        let args = bootstrap.args.as_ref().unwrap();
        assert!(args[0].contains("/usr/local/bin/pulumi-kubernetes-operator"));
        assert!(args[0].contains("/usr/bin/tini"));
    }

    #[test]
    fn test_extra_env_propagated_to_main_container() {
        let ws = make_workspace("test-ws", None);
        let extra_env = vec![
            EnvVar {
                name: "PULUMI_BACKEND_URL".to_owned(),
                value: Some("file:///share/state".to_owned()),
                ..Default::default()
            },
            EnvVar {
                name: "GOOGLE_CREDENTIALS".to_owned(),
                value_from: Some(EnvVarSource {
                    secret_key_ref: Some(SecretKeySelector {
                        name: "gcp-credentials".to_owned(),
                        key: "credentials.json".to_owned(),
                        optional: Some(false),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ];
        let sts = build_statefulset(&ws, "test-ws", "default", test_owner_ref(), TEST_IMAGE, extra_env, None);

        let pod_spec = sts.spec.as_ref().unwrap().template.spec.as_ref().unwrap();
        let pulumi = &pod_spec.containers[0];
        let env = pulumi.env.as_ref().expect("pulumi container should have env vars");
        assert_eq!(env.len(), 2);
        assert_eq!(env[0].name, "PULUMI_BACKEND_URL");
        assert_eq!(env[0].value.as_deref(), Some("file:///share/state"));
        assert_eq!(env[1].name, "GOOGLE_CREDENTIALS");
        assert!(env[1].value_from.is_some());
    }

    #[test]
    fn test_agent_image_env_var() {
        // When AGENT_IMAGE is not set, falls back to dev default
        std::env::remove_var("AGENT_IMAGE");
        assert_eq!(agent_image(), "pulumi-kubernetes-operator:dev");

        std::env::set_var("AGENT_IMAGE", "my-registry/pko:v1.0.0");
        assert_eq!(agent_image(), "my-registry/pko:v1.0.0");
        std::env::remove_var("AGENT_IMAGE");
    }

    #[test]
    fn test_workspace_ready_condition() {
        let mut sts = StatefulSet::default();
        assert!(!is_statefulset_ready(&sts));

        sts.metadata.generation = Some(1);
        sts.status = Some(k8s_openapi::api::apps::v1::StatefulSetStatus {
            observed_generation: Some(1),
            update_revision: Some("rev1".to_owned()),
            current_revision: Some("rev1".to_owned()),
            available_replicas: Some(1),
            ..Default::default()
        });
        assert!(is_statefulset_ready(&sts));
    }

    #[test]
    fn test_workspace_rolling_update() {
        let mut sts = StatefulSet::default();
        sts.metadata.generation = Some(2);
        sts.status = Some(k8s_openapi::api::apps::v1::StatefulSetStatus {
            observed_generation: Some(2),
            update_revision: Some("rev2".to_owned()),
            current_revision: Some("rev1".to_owned()), // Different = rolling
            available_replicas: Some(1),
            ..Default::default()
        });
        assert!(!is_statefulset_ready(&sts));
    }

    #[test]
    fn test_workspace_address() {
        let addr = get_workspace_address("my-stack", "prod");
        assert_eq!(addr, "my-stack-workspace.prod:50051");
    }

    #[test]
    fn test_build_env_vars_with_secret_ref() {
        use crate::api::stack::{ResourceRef, SecretSelector as StackSecretSelector};

        let mut env_refs_map = BTreeMap::new();
        env_refs_map.insert("GOOGLE_CREDENTIALS".to_owned(), ResourceRef {
            selector_type: ResourceSelectorType::Secret,
            filesystem: None,
            env: None,
            secret_ref: Some(StackSecretSelector {
                namespace: None,
                name: "gcp-creds".to_owned(),
                key: "creds.json".to_owned(),
            }),
            literal_ref: None,
        });

        let mut stack = Stack::new("test", make_stack_spec());
        stack.spec.backend = Some("file:///share/state".to_owned());
        stack.spec.env_refs = Some(env_refs_map);
        stack.metadata.namespace = Some("test-ns".to_owned());

        let envs = build_env_vars(&stack);
        assert_eq!(envs.len(), 2); // PULUMI_BACKEND_URL + GOOGLE_CREDENTIALS

        let backend_env = envs.iter().find(|e| e.name == "PULUMI_BACKEND_URL").unwrap();
        assert_eq!(backend_env.value.as_deref(), Some("file:///share/state"));

        let creds_env = envs.iter().find(|e| e.name == "GOOGLE_CREDENTIALS").unwrap();
        let secret_ref = creds_env.value_from.as_ref().unwrap().secret_key_ref.as_ref().unwrap();
        assert_eq!(secret_ref.name.as_str(), "gcp-creds");
        assert_eq!(secret_ref.key, "creds.json");
    }

    /// Helper to create a minimal StackSpec for testing.
    fn make_stack_spec() -> crate::api::stack::StackSpec {
        crate::api::stack::StackSpec {
            stack: "dev".to_owned(),
            backend: None,
            project_repo: None,
            flux_source: None,
            program_ref: None,
            branch: None,
            commit: None,
            repo_dir: None,
            shallow: false,
            git_auth: None,
            git_auth_secret: None,
            config: None,
            secrets: None,
            config_ref: None,
            secret_refs: None,
            secrets_provider: None,
            access_token_secret: None,
            env_refs: None,
            envs: vec![],
            secret_envs: vec![],
            refresh: false,
            expect_no_refresh_changes: false,
            destroy_on_finalize: false,
            retry_on_update_conflict: false,
            continue_resync_on_commit_match: false,
            use_local_stack_only: false,
            resync_frequency_seconds: 0,
            preview: false,
            targets: vec![],
            target_dependents: false,
            prerequisites: vec![],
            service_account_name: None,
            workspace_template: None,
            workspace_reclaim_policy: crate::api::stack::WorkspaceReclaimPolicy::Retain,
            environment: vec![],
            update_template: None,
            retry_max_backoff_duration_seconds: 0,
            lock_timeout_seconds: 600,
            operation_timeout_seconds: 3600,
            finalizer_timeout_seconds: 3600,
            project_verification: None,
        }
    }
}
