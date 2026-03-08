use std::collections::HashMap;

use parking_lot::Mutex;

use kube::ResourceExt;
use sha2::{Digest, Sha256};

use crate::api::program::{Artifact, Program, ProgramSpec};
use crate::errors::OperatorError;
use crate::operator::finalizers::{program_finalizer_action, ProgramFinalizerAction};
use crate::operator::manager::Manager;

/// Reconcile a Program CR.
pub async fn reconcile_program(
    mgr: &Manager,
    program: &Program,
) -> Result<ProgramReconcileAction, OperatorError> {
    let ns = program.namespace().unwrap_or_else(|| "default".to_owned());

    // Use the shared store for efficient lookup -- count only, no cloning
    let referencing_count = count_referencing_stacks(mgr, &ns, &program.name_any());

    match program_finalizer_action(program, referencing_count) {
        ProgramFinalizerAction::Add => Ok(ProgramReconcileAction::AddFinalizer),
        ProgramFinalizerAction::None => Ok(ProgramReconcileAction::EnsureServing),
        ProgramFinalizerAction::Remove => Ok(ProgramReconcileAction::RemoveFinalizer),
        ProgramFinalizerAction::Block => {
            Ok(ProgramReconcileAction::BlockDeletion { referencing_count })
        }
    }
}

pub enum ProgramReconcileAction {
    AddFinalizer,
    RemoveFinalizer,
    EnsureServing,
    BlockDeletion { referencing_count: usize },
}

fn count_referencing_stacks(mgr: &Manager, ns: &str, program_name: &str) -> usize {
    mgr.stores
        .stacks
        .state()
        .iter()
        .filter(|s| {
            s.namespace().as_deref() == Some(ns)
                && s.spec
                    .program_ref
                    .as_ref()
                    .is_some_and(|pr| pr.name == program_name)
        })
        .count()
}

/// Build a program artifact: serialize ProgramSpec -> Pulumi YAML, create tar.gz in memory.
pub fn build_artifact(
    spec: &ProgramSpec,
    ns: &str,
    name: &str,
    generation: i64,
    server_addr: &str,
) -> Result<(Artifact, Vec<u8>), OperatorError> {
    // Serialize to Pulumi YAML with runtime: yaml
    let yaml_content = serialize_program_yaml(spec)?;

    // Create tar.gz in memory
    let tar_gz = create_tar_gz("Pulumi.yaml", yaml_content.as_bytes())?;

    // Compute SHA256 digest
    let mut hasher = Sha256::new();
    hasher.update(&tar_gz);
    let digest = format!("sha256:{}", hex::encode(hasher.finalize()));

    let gen_str = generation.to_string();
    let path = format!("programs/{}/{}/{}.tar.gz", ns, name, gen_str);
    let url = format!("http://{}/{}", server_addr, path);

    let artifact = Artifact {
        path,
        url,
        revision: gen_str,
        digest: Some(digest),
        last_update_time: chrono::Utc::now().to_rfc3339(),
        size: Some(tar_gz.len() as i64),
        metadata: None,
    };

    Ok((artifact, tar_gz))
}

/// Serialize ProgramSpec to Pulumi YAML format.
fn serialize_program_yaml(spec: &ProgramSpec) -> Result<String, OperatorError> {
    use serde_yaml::Value;

    let mut doc = serde_yaml::Mapping::new();
    doc.insert(
        Value::String("runtime".to_owned()),
        Value::String("yaml".to_owned()),
    );
    doc.insert(
        Value::String("name".to_owned()),
        Value::String("program".to_owned()),
    );

    if let Some(ref config) = spec.configuration {
        let config_val = serde_yaml::to_value(config).map_err(|_| {
            OperatorError::Permanent(crate::errors::PermanentError::SpecInvalid {
                field: "configuration",
            })
        })?;
        doc.insert(Value::String("config".to_owned()), config_val);
    }

    if let Some(ref resources) = spec.resources {
        let res_val = serde_yaml::to_value(resources).map_err(|_| {
            OperatorError::Permanent(crate::errors::PermanentError::SpecInvalid {
                field: "resources",
            })
        })?;
        doc.insert(Value::String("resources".to_owned()), res_val);
    }

    if let Some(ref variables) = spec.variables {
        let var_val = serde_yaml::to_value(variables).map_err(|_| {
            OperatorError::Permanent(crate::errors::PermanentError::SpecInvalid {
                field: "variables",
            })
        })?;
        doc.insert(Value::String("variables".to_owned()), var_val);
    }

    if let Some(ref outputs) = spec.outputs {
        let out_val = serde_yaml::to_value(outputs).map_err(|_| {
            OperatorError::Permanent(crate::errors::PermanentError::SpecInvalid {
                field: "outputs",
            })
        })?;
        doc.insert(Value::String("outputs".to_owned()), out_val);
    }

    // Emit packages as a top-level key, matching the official Go operator behavior.
    // Pulumi uses this to resolve parameterized packages and VCS-based components.
    if let Some(ref packages) = spec.packages {
        let pkg_val = serde_yaml::to_value(packages).map_err(|_| {
            OperatorError::Permanent(crate::errors::PermanentError::SpecInvalid {
                field: "packages",
            })
        })?;
        doc.insert(Value::String("packages".to_owned()), pkg_val);
    }

    serde_yaml::to_string(&Value::Mapping(doc)).map_err(|_| {
        OperatorError::Permanent(crate::errors::PermanentError::SpecInvalid { field: "program" })
    })
}

/// Create a tar.gz archive containing a single file.
fn create_tar_gz(filename: &str, content: &[u8]) -> Result<Vec<u8>, OperatorError> {
    let mut gz_buf = Vec::new();
    {
        let gz_encoder = flate2::write::GzEncoder::new(&mut gz_buf, flate2::Compression::default());
        let mut tar_builder = tar::Builder::new(gz_encoder);

        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();

        tar_builder
            .append_data(&mut header, filename, content)
            .map_err(|_| {
                OperatorError::Permanent(crate::errors::PermanentError::SpecInvalid {
                    field: "program",
                })
            })?;

        tar_builder
            .into_inner()
            .map_err(|_| {
                OperatorError::Permanent(crate::errors::PermanentError::SpecInvalid {
                    field: "program",
                })
            })?
            .finish()
            .map_err(|_| {
                OperatorError::Permanent(crate::errors::PermanentError::SpecInvalid {
                    field: "program",
                })
            })?;
    }

    Ok(gz_buf)
}

/// In-memory program file server. Thread-safe via Mutex.
pub struct ProgramFileServer {
    artifacts: Mutex<HashMap<String, Vec<u8>>>,
}

impl Default for ProgramFileServer {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgramFileServer {
    pub fn new() -> Self {
        Self {
            artifacts: Mutex::new(HashMap::new()),
        }
    }

    pub fn store_artifact(&self, path: &str, data: Vec<u8>) {
        self.artifacts.lock().insert(path.to_owned(), data);
    }

    pub fn get_artifact(&self, path: &str) -> Option<Vec<u8>> {
        self.artifacts.lock().get(path).cloned()
    }

    pub fn remove_artifact(&self, path: &str) {
        self.artifacts.lock().remove(path);
    }
}

/// Serve program artifacts over HTTP on the given port.
/// Used by workspace init containers to download program sources.
pub async fn serve_file_server(
    server: &'static ProgramFileServer,
    port: u16,
) -> Result<(), crate::errors::RunError> {
    use std::convert::Infallible;
    use std::net::SocketAddr;

    use http_body_util::Full;
    use hyper::body::Bytes;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper::{Request, Response, StatusCode};
    use hyper_util::rt::TokioIo;
    use tokio::net::TcpListener;

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "program file server listening");

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        tokio::spawn(async move {
            let svc = service_fn(move |req: Request<hyper::body::Incoming>| async move {
                let path = req.uri().path().trim_start_matches('/');
                match server.get_artifact(path) {
                    Some(data) => {
                        let resp: Response<Full<Bytes>> = Response::builder()
                            .header("Content-Type", "application/gzip")
                            .body(Full::new(Bytes::from(data)))
                            .unwrap();
                        Ok::<_, Infallible>(resp)
                    }
                    None => {
                        let resp: Response<Full<Bytes>> = Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body(Full::new(Bytes::from("not found")))
                            .unwrap();
                        Ok(resp)
                    }
                }
            });
            if let Err(e) = http1::Builder::new().serve_connection(io, svc).await {
                tracing::debug!(error = %e, "file server connection error");
            }
        });
    }
}

/// The port the program file server listens on.
pub const FILE_SERVER_PORT: u16 = 9090;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn make_program_spec() -> ProgramSpec {
        ProgramSpec {
            configuration: None,
            resources: Some({
                let mut map = BTreeMap::new();
                map.insert(
                    "bucket".to_owned(),
                    crate::api::program::Resource {
                        resource_type: "gcp:storage:Bucket".to_owned(),
                        properties: Some({
                            let mut props = BTreeMap::new();
                            props.insert(
                                "location".to_owned(),
                                serde_json::Value::String("US".to_owned()),
                            );
                            props
                        }),
                        options: None,
                        get: None,
                    },
                );
                map
            }),
            variables: None,
            outputs: Some({
                let mut map = BTreeMap::new();
                map.insert(
                    "bucketName".to_owned(),
                    serde_json::Value::String("${bucket.name}".to_owned()),
                );
                map
            }),
            packages: Some({
                let mut map = BTreeMap::new();
                map.insert("gcp".to_owned(), "7.0.0".to_owned());
                map
            }),
        }
    }

    #[test]
    fn test_program_artifact_generation() {
        let spec = make_program_spec();
        let (artifact, data) =
            build_artifact(&spec, "test-ns", "my-prog", 1, "localhost:8080").unwrap();

        assert!(!data.is_empty());
        assert_eq!(artifact.revision, "1");
        assert!(artifact.path.contains("programs/test-ns/my-prog/1.tar.gz"));

        // Verify it's valid gzip
        use flate2::read::GzDecoder;
        use std::io::Read;
        let mut decoder = GzDecoder::new(&data[..]);
        let mut decoded = Vec::new();
        decoder.read_to_end(&mut decoded).unwrap();

        // Verify it's valid tar with Pulumi.yaml
        let mut archive = tar::Archive::new(&decoded[..]);
        let entries: Vec<_> = archive.entries().unwrap().collect();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_program_artifact_digest() {
        let spec = make_program_spec();
        let (artifact, data) =
            build_artifact(&spec, "test-ns", "my-prog", 1, "localhost:8080").unwrap();

        // Verify digest matches
        let mut hasher = Sha256::new();
        hasher.update(&data);
        let expected = format!("sha256:{}", hex::encode(hasher.finalize()));
        assert_eq!(artifact.digest.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn test_program_artifact_url() {
        let spec = make_program_spec();
        let (artifact, _) =
            build_artifact(&spec, "test-ns", "my-prog", 3, "10.0.0.1:8080").unwrap();

        assert_eq!(
            artifact.url,
            "http://10.0.0.1:8080/programs/test-ns/my-prog/3.tar.gz"
        );
    }

    #[test]
    fn test_program_packages_included() {
        let spec = make_program_spec();
        let yaml = serialize_program_yaml(&spec).unwrap();
        assert!(yaml.contains("runtime: yaml"));
        assert!(yaml.contains("gcp"));
    }

    #[test]
    fn test_program_generation_revision() {
        let spec = make_program_spec();
        let (artifact, _) = build_artifact(&spec, "ns", "prog", 42, "addr:8080").unwrap();
        assert_eq!(artifact.revision, "42");
    }

    #[test]
    fn test_program_file_server() {
        let server = ProgramFileServer::new();
        server.store_artifact("test/path.tar.gz", vec![1, 2, 3]);
        assert_eq!(server.get_artifact("test/path.tar.gz"), Some(vec![1, 2, 3]));
        assert_eq!(server.get_artifact("missing"), None);

        server.remove_artifact("test/path.tar.gz");
        assert_eq!(server.get_artifact("test/path.tar.gz"), None);
    }
}
