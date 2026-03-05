use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::task::Poll;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tonic::{Request, Response, Status};

use crate::agent::redact::redact_stderr;
use crate::proto::agent::automation_service_server::{AutomationService, AutomationServiceServer};
use crate::proto::agent::*;

/// Maximum bytes accumulated from subprocess stdout/stderr before truncation.
const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

pub struct AgentServer {
    workspace_dir: PathBuf,
    log_level: u32,
}

impl AgentServer {
    pub fn new(workspace_dir: &str, log_level: u32) -> Self {
        Self {
            workspace_dir: PathBuf::from(workspace_dir),
            log_level,
        }
    }

    fn pulumi_cmd(&self) -> Command {
        let mut cmd = Command::new("pulumi");
        cmd.current_dir(&self.workspace_dir);
        cmd.env("PULUMI_SKIP_UPDATE_CHECK", "true");
        if self.log_level > 0 {
            cmd.arg("-v").arg(self.log_level.to_string());
        }
        cmd
    }

    async fn run_pulumi(&self, args: &[&str]) -> Result<String, Status> {
        let output = self
            .pulumi_cmd()
            .args(args)
            .output()
            .await
            .map_err(|e| Status::internal(format!("failed to run pulumi: {}", e)))?;

        if output.status.success() {
            String::from_utf8(output.stdout)
                .map_err(|e| Status::internal(format!("invalid output: {}", e)))
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(Status::internal(format!(
                "pulumi failed: {}",
                redact_stderr(&stderr)
            )))
        }
    }
}

/// Detect if any config value is non-string (object/array/number/boolean).
/// If so, use JSON config mode. Otherwise use path config mode.
#[cfg(test)]
fn has_non_string_values(config: &[ConfigItem]) -> bool {
    config.iter().any(|item| {
        if let Some(config_item::V::Value(ref value)) = item.v {
            if let Some(ref kind) = value.kind {
                !matches!(kind, prost_types::value::Kind::StringValue(_))
            } else {
                false
            }
        } else {
            false
        }
    })
}

/// Namespace a config key: if no `:` in key, prepend `{project}:`.
#[cfg(test)]
fn namespace_key(key: &str, project: &str) -> String {
    if key.contains(':') {
        key.to_owned()
    } else {
        format!("{}:{}", project, key)
    }
}

/// Escape a key for path mode: key -> ["key"] if not already escaped.
#[cfg(test)]
fn escape_path_key(key: &str) -> String {
    if key.starts_with('[') {
        key.to_owned()
    } else {
        format!("[\"{}\"]", key)
    }
}

pin_project_lite::pin_project! {
    /// Stream wrapper that cancels a CancellationToken on drop,
    /// killing the Pulumi subprocess when the gRPC client disconnects.
    pub struct CancellableStream<S> {
        #[pin]
        inner: S,
        _guard: tokio_util::sync::DropGuard,
    }
}

impl<S> CancellableStream<S> {
    fn new(inner: S, cancel: tokio_util::sync::CancellationToken) -> Self {
        Self {
            inner,
            _guard: cancel.drop_guard(),
        }
    }
}

impl<S: futures::Stream> futures::Stream for CancellableStream<S> {
    type Item = S::Item;
    fn poll_next(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().inner.poll_next(cx)
    }
}

#[tonic::async_trait]
impl AutomationService for AgentServer {
    async fn who_am_i(
        &self,
        _request: Request<WhoAmIRequest>,
    ) -> Result<Response<WhoAmIResult>, Status> {
        let output = self.run_pulumi(&["whoami", "--json"]).await?;
        let parsed: serde_json::Value = serde_json::from_str(&output)
            .map_err(|e| Status::internal(format!("failed to parse whoami: {}", e)))?;

        Ok(Response::new(WhoAmIResult {
            user: parsed["user"].as_str().unwrap_or("").to_owned(),
            organizations: parsed["organizations"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_owned()))
                        .collect()
                })
                .unwrap_or_default(),
            url: parsed["url"].as_str().unwrap_or("").to_owned(),
        }))
    }

    async fn select_stack(
        &self,
        request: Request<SelectStackRequest>,
    ) -> Result<Response<SelectStackResult>, Status> {
        let req = request.into_inner();
        let create = req.create.unwrap_or(false);
        tracing::info!(stack = %req.stack_name, create, "SelectStack");

        let mut args = vec!["stack", "select"];
        if create {
            args.push("--create");
        }
        args.push(&req.stack_name);

        self.run_pulumi(&args).await?;
        Ok(Response::new(SelectStackResult { summary: None }))
    }

    async fn info(
        &self,
        _request: Request<InfoRequest>,
    ) -> Result<Response<InfoResult>, Status> {
        Ok(Response::new(InfoResult { summary: None }))
    }

    async fn set_all_config(
        &self,
        request: Request<SetAllConfigRequest>,
    ) -> Result<Response<SetAllConfigResult>, Status> {
        let req = request.into_inner();
        tracing::info!(count = req.config.len(), "SetAllConfig");

        // Config items are set via `pulumi config set` for each item
        for item in &req.config {
            let mut args: Vec<&str> = vec!["config", "set"];
            if item.secret.unwrap_or(false) {
                args.push("--secret");
            }
            if item.path.unwrap_or(false) {
                args.push("--path");
            }

            args.push(&item.key);

            // Extract value from oneof v field
            let val_str;
            if let Some(ref v) = item.v {
                match v {
                    config_item::V::Value(value) => {
                        if let Some(ref kind) = value.kind {
                            val_str = match kind {
                                prost_types::value::Kind::StringValue(s) => s.clone(),
                                prost_types::value::Kind::NumberValue(n) => n.to_string(),
                                prost_types::value::Kind::BoolValue(b) => b.to_string(),
                                _ => prost_value_to_json_string(value),
                            };
                            args.push(&val_str);
                        }
                    }
                    config_item::V::ValueFrom(_vf) => {
                        // ValueFrom resolved by agent init; skip here
                    }
                }
            }

            self.run_pulumi(&args).await?;
        }

        Ok(Response::new(SetAllConfigResult {}))
    }

    async fn add_environments(
        &self,
        request: Request<AddEnvironmentsRequest>,
    ) -> Result<Response<AddEnvironmentsResult>, Status> {
        let req = request.into_inner();
        tracing::info!(count = req.environment.len(), "AddEnvironments");

        for env in &req.environment {
            self.run_pulumi(&["config", "env", "add", env]).await?;
        }

        Ok(Response::new(AddEnvironmentsResult {}))
    }

    async fn install(
        &self,
        _request: Request<InstallRequest>,
    ) -> Result<Response<InstallResult>, Status> {
        tracing::info!("Install: running pulumi install");
        self.run_pulumi(&["install"]).await?;
        Ok(Response::new(InstallResult {}))
    }

    type PreviewStream =
        CancellableStream<tokio_stream::wrappers::ReceiverStream<Result<PreviewStream, Status>>>;

    async fn preview(
        &self,
        request: Request<PreviewRequest>,
    ) -> Result<Response<Self::PreviewStream>, Status> {
        let req = request.into_inner();
        tracing::info!("Preview: starting");

        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let workspace_dir = self.workspace_dir.clone();
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_clone = cancel.clone();

        tokio::spawn(async move {
            let parallel_str = req.parallel.map(|p| p.to_string());

            let mut args: Vec<&str> = vec!["preview", "--diff", "--json", "--non-interactive"];
            if let Some(ref p) = parallel_str {
                args.extend(["--parallel", p.as_str()]);
            }
            if let Some(ref msg) = req.message {
                args.extend(["--message", msg.as_str()]);
            }
            if req.expect_no_changes == Some(true) {
                args.push("--expect-no-changes");
            }
            for r in &req.replace {
                args.extend(["--replace", r.as_str()]);
            }
            for t in &req.target {
                args.extend(["--target", t.as_str()]);
            }
            if req.target_dependents == Some(true) {
                args.push("--target-dependents");
            }
            if req.refresh == Some(true) {
                args.push("--refresh");
            }

            let result = run_pulumi_streaming(&workspace_dir, &args, cancel_clone).await;
            let _ = tx
                .send(Ok(crate::proto::agent::PreviewStream {
                    response: Some(
                        crate::proto::agent::preview_stream::Response::Result(PreviewResult {
                            stdout: result.stdout,
                            stderr: redact_stderr(&result.stderr).into_owned(),
                            summary: None,
                            permalink: result.permalink,
                        }),
                    ),
                }))
                .await;
        });

        Ok(Response::new(CancellableStream::new(
            tokio_stream::wrappers::ReceiverStream::new(rx),
            cancel,
        )))
    }

    type RefreshStream =
        CancellableStream<tokio_stream::wrappers::ReceiverStream<Result<RefreshStream, Status>>>;

    async fn refresh(
        &self,
        request: Request<RefreshRequest>,
    ) -> Result<Response<Self::RefreshStream>, Status> {
        let req = request.into_inner();
        tracing::info!("Refresh: starting");

        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let workspace_dir = self.workspace_dir.clone();
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_clone = cancel.clone();

        tokio::spawn(async move {
            let parallel_str = req.parallel.map(|p| p.to_string());

            let mut args: Vec<&str> = vec!["refresh", "--yes", "--non-interactive", "--json"];
            if let Some(ref p) = parallel_str {
                args.extend(["--parallel", p.as_str()]);
            }
            if let Some(ref msg) = req.message {
                args.extend(["--message", msg.as_str()]);
            }
            if req.expect_no_changes == Some(true) {
                args.push("--expect-no-changes");
            }
            for t in &req.target {
                args.extend(["--target", t.as_str()]);
            }

            let result = run_pulumi_streaming(&workspace_dir, &args, cancel_clone).await;
            let _ = tx
                .send(Ok(crate::proto::agent::RefreshStream {
                    response: Some(
                        crate::proto::agent::refresh_stream::Response::Result(RefreshResult {
                            stdout: result.stdout,
                            stderr: redact_stderr(&result.stderr).into_owned(),
                            summary: None,
                            permalink: result.permalink,
                        }),
                    ),
                }))
                .await;
        });

        Ok(Response::new(CancellableStream::new(
            tokio_stream::wrappers::ReceiverStream::new(rx),
            cancel,
        )))
    }

    type UpStream = CancellableStream<tokio_stream::wrappers::ReceiverStream<Result<UpStream, Status>>>;

    async fn up(
        &self,
        request: Request<UpRequest>,
    ) -> Result<Response<Self::UpStream>, Status> {
        let req = request.into_inner();
        tracing::info!("Up: starting");

        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let workspace_dir = self.workspace_dir.clone();
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_clone = cancel.clone();

        tokio::spawn(async move {
            let parallel_str = req.parallel.map(|p| p.to_string());

            let mut args: Vec<&str> = vec!["up", "--yes", "--non-interactive", "--suppress-progress", "--json"];
            if let Some(ref p) = parallel_str {
                args.extend(["--parallel", p.as_str()]);
            }
            if let Some(ref msg) = req.message {
                args.extend(["--message", msg.as_str()]);
            }
            if req.expect_no_changes == Some(true) {
                args.push("--expect-no-changes");
            }
            for r in &req.replace {
                args.extend(["--replace", r.as_str()]);
            }
            for t in &req.target {
                args.extend(["--target", t.as_str()]);
            }
            if req.target_dependents == Some(true) {
                args.push("--target-dependents");
            }
            if req.refresh == Some(true) {
                args.push("--refresh");
            }
            if req.continue_on_error == Some(true) {
                args.push("--continue-on-error");
            }

            let result = run_pulumi_streaming(&workspace_dir, &args, cancel_clone).await;
            if result.failed {
                let redacted = redact_stderr(&result.stderr);
                tracing::error!(stderr = %redacted, "Up: pulumi command failed");
                let _ = tx
                    .send(Err(Status::internal(format!(
                        "pulumi up failed: {}",
                        if redacted.is_empty() {
                            "unknown error"
                        } else {
                            redacted.trim()
                        }
                    ))))
                    .await;
            } else {
                tracing::info!("Up: completed successfully");
                let _ = tx
                    .send(Ok(crate::proto::agent::UpStream {
                        response: Some(crate::proto::agent::up_stream::Response::Result(UpResult {
                            stdout: result.stdout,
                            stderr: redact_stderr(&result.stderr).into_owned(),
                            summary: None,
                            permalink: result.permalink,
                            outputs: result.outputs,
                        })),
                    }))
                    .await;
            }
        });

        Ok(Response::new(CancellableStream::new(
            tokio_stream::wrappers::ReceiverStream::new(rx),
            cancel,
        )))
    }

    type DestroyStream =
        CancellableStream<tokio_stream::wrappers::ReceiverStream<Result<DestroyStream, Status>>>;

    async fn destroy(
        &self,
        request: Request<DestroyRequest>,
    ) -> Result<Response<Self::DestroyStream>, Status> {
        let req = request.into_inner();
        tracing::info!("Destroy: starting");

        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let workspace_dir = self.workspace_dir.clone();
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_clone = cancel.clone();

        tokio::spawn(async move {
            let parallel_str = req.parallel.map(|p| p.to_string());

            let mut args: Vec<&str> = vec!["destroy", "--yes", "--non-interactive", "--json"];
            if let Some(ref p) = parallel_str {
                args.extend(["--parallel", p.as_str()]);
            }
            if let Some(ref msg) = req.message {
                args.extend(["--message", msg.as_str()]);
            }
            for t in &req.target {
                args.extend(["--target", t.as_str()]);
            }
            if req.target_dependents == Some(true) {
                args.push("--target-dependents");
            }
            if req.refresh == Some(true) {
                args.push("--refresh");
            }
            if req.continue_on_error == Some(true) {
                args.push("--continue-on-error");
            }
            if req.remove == Some(true) {
                args.push("--remove");
            }

            let result = run_pulumi_streaming(&workspace_dir, &args, cancel_clone).await;
            if result.failed {
                let redacted = redact_stderr(&result.stderr);
                tracing::error!(stderr = %redacted, "Destroy: pulumi command failed");
                let _ = tx
                    .send(Err(Status::internal(format!(
                        "pulumi destroy failed: {}",
                        if redacted.is_empty() {
                            "unknown error"
                        } else {
                            redacted.trim()
                        }
                    ))))
                    .await;
            } else {
                tracing::info!("Destroy: completed successfully");
                let _ = tx
                    .send(Ok(crate::proto::agent::DestroyStream {
                        response: Some(
                            crate::proto::agent::destroy_stream::Response::Result(DestroyResult {
                                stdout: result.stdout,
                                stderr: redact_stderr(&result.stderr).into_owned(),
                                summary: None,
                                permalink: result.permalink,
                            }),
                        ),
                    }))
                    .await;
            }
        });

        Ok(Response::new(CancellableStream::new(
            tokio_stream::wrappers::ReceiverStream::new(rx),
            cancel,
        )))
    }

    async fn pulumi_version(
        &self,
        _request: Request<PulumiVersionRequest>,
    ) -> Result<Response<PulumiVersionResult>, Status> {
        let output = self.run_pulumi(&["version"]).await?;
        Ok(Response::new(PulumiVersionResult {
            version: output.trim().to_owned(),
        }))
    }

    async fn cancel_update(
        &self,
        _request: Request<CancelUpdateRequest>,
    ) -> Result<Response<CancelUpdateResult>, Status> {
        tracing::info!("CancelUpdate: force-unlocking");
        self.run_pulumi(&["cancel", "--yes"]).await?;
        Ok(Response::new(CancelUpdateResult {
            message: "Lock cleared".to_owned(),
        }))
    }
}

struct PulumiResult {
    stdout: String,
    stderr: String,
    permalink: Option<String>,
    outputs: HashMap<String, OutputValue>,
    failed: bool,
}

async fn run_pulumi_streaming(
    workspace_dir: &std::path::Path,
    args: &[&str],
    cancel: tokio_util::sync::CancellationToken,
) -> PulumiResult {
    let mut child = match Command::new("pulumi")
        .args(args)
        .current_dir(workspace_dir)
        .env("PULUMI_SKIP_UPDATE_CHECK", "true")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return PulumiResult {
                stdout: String::new(),
                stderr: format!("failed to spawn pulumi: {}", e),
                permalink: None,
                outputs: HashMap::new(),
                failed: true,
            };
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let mut all_stdout = String::new();
    let mut all_stderr = String::new();
    let mut permalink = None;
    let mut stdout_truncated = false;
    let mut stderr_truncated = false;

    if let Some(stdout) = stdout {
        let mut reader = BufReader::new(stdout).lines();
        loop {
            tokio::select! {
                line = reader.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) {
                                if let Some(link) = event.get("permalink").and_then(|v| v.as_str()) {
                                    permalink = Some(link.to_owned());
                                }
                            }
                            if all_stdout.len() < MAX_OUTPUT_BYTES {
                                all_stdout.push_str(&line);
                                all_stdout.push('\n');
                            } else if !stdout_truncated {
                                stdout_truncated = true;
                                tracing::warn!(limit = MAX_OUTPUT_BYTES, "pulumi stdout truncated");
                            }
                        }
                        _ => break,
                    }
                }
                _ = cancel.cancelled() => {
                    tracing::warn!("pulumi process cancelled, killing child");
                    let _ = child.kill().await;
                    return PulumiResult {
                        stdout: all_stdout,
                        stderr: "operation cancelled by operator".to_owned(),
                        permalink,
                        outputs: HashMap::new(),
                        failed: true,
                    };
                }
            }
        }
    }

    if let Some(stderr) = stderr {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if all_stderr.len() < MAX_OUTPUT_BYTES {
                all_stderr.push_str(&line);
                all_stderr.push('\n');
            } else if !stderr_truncated {
                stderr_truncated = true;
                tracing::warn!(limit = MAX_OUTPUT_BYTES, "pulumi stderr truncated");
            }
        }
    }

    let exit_status = child.wait().await;

    // Parse outputs from the JSON stdout (pulumi up --json emits a final summary)
    let mut outputs = HashMap::new();
    if let Some(last_line) = all_stdout.lines().rev().find(|l| l.starts_with('{')) {
        if let Ok(summary) = serde_json::from_str::<serde_json::Value>(last_line) {
            if let Some(out_map) = summary.get("outputs").and_then(|o| o.as_object()) {
                for (k, v) in out_map {
                    outputs.insert(
                        k.clone(),
                        OutputValue {
                            value: v.to_string().into_bytes().into(),
                            secret: false,
                        },
                    );
                }
            }
        }
    }

    // Check exit code to detect failures
    let failed = match exit_status {
        Ok(status) => !status.success(),
        Err(_) => true,
    };

    if failed && all_stderr.is_empty() {
        // When using --json, errors appear in stdout; extract diagnostic lines
        let diagnostics: Vec<&str> = all_stdout
            .lines()
            .filter(|l| l.contains("error:") || l.contains("Error"))
            .take(5)
            .collect();
        if diagnostics.is_empty() {
            all_stderr = "pulumi command failed (non-zero exit code)".to_owned();
        } else {
            all_stderr = diagnostics.join("; ");
        }
    }

    PulumiResult {
        stdout: all_stdout,
        stderr: all_stderr,
        permalink,
        outputs,
        failed,
    }
}

/// Convert a prost Value to a JSON-ish string for CLI arguments.
fn prost_value_to_json_string(value: &prost_types::Value) -> String {
    use prost_types::value::Kind;
    match &value.kind {
        Some(Kind::NullValue(_)) => "null".to_owned(),
        Some(Kind::NumberValue(n)) => n.to_string(),
        Some(Kind::StringValue(s)) => s.clone(),
        Some(Kind::BoolValue(b)) => b.to_string(),
        Some(Kind::StructValue(s)) => {
            // Manual JSON serialization for prost Struct
            let pairs: Vec<String> = s.fields.iter().map(|(k, v)| {
                format!("\"{}\":{}", k, prost_value_to_json_string(v))
            }).collect();
            format!("{{{}}}", pairs.join(","))
        }
        Some(Kind::ListValue(l)) => {
            let items: Vec<String> = l.values.iter().map(prost_value_to_json_string).collect();
            format!("[{}]", items.join(","))
        }
        None => "null".to_owned(),
    }
}

/// Validate that a backend URL uses an allowed scheme.
fn validate_backend(url: &str) -> Result<(), crate::errors::RunError> {
    const ALLOWED_SCHEMES: &[&str] = &["file://", "gs://", "s3://", "azblob://", "https://"];
    if !ALLOWED_SCHEMES.iter().any(|s| url.starts_with(s)) {
        return Err(crate::errors::RunError::Generic(format!(
            "unsupported backend scheme in PULUMI_BACKEND_URL: {}",
            url.split("://").next().unwrap_or("unknown")
        )));
    }
    Ok(())
}

pub async fn run_server(
    listen_address: &str,
    workspace_dir: &str,
    log_level: u32,
) -> Result<(), crate::errors::RunError> {
    // Ensure the file backend directory exists if PULUMI_BACKEND_URL uses file://
    if let Ok(backend_url) = std::env::var("PULUMI_BACKEND_URL") {
        validate_backend(&backend_url)?;
        if let Some(path) = backend_url.strip_prefix("file://") {
            let state_dir = std::path::Path::new(path);
            if !state_dir.exists() {
                tokio::fs::create_dir_all(state_dir).await?;
                tracing::info!(dir = %path, "created file backend state directory");
            }
        }
    }

    let addr = listen_address.parse()?;
    let server = AgentServer::new(workspace_dir, log_level);

    tracing::info!(%listen_address, "starting gRPC server");

    tonic::transport::Server::builder()
        .add_service(
            AutomationServiceServer::new(server)
                .max_decoding_message_size(4 * 1024 * 1024)  // 4 MiB
                .max_encoding_message_size(16 * 1024 * 1024), // 16 MiB (state can be large)
        )
        .serve(addr)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_key_namespacing() {
        assert_eq!(namespace_key("myKey", "myproject"), "myproject:myKey");
        assert_eq!(namespace_key("ns:myKey", "myproject"), "ns:myKey");
    }

    #[test]
    fn test_config_path_escaping() {
        assert_eq!(escape_path_key("myKey"), "[\"myKey\"]");
        assert_eq!(escape_path_key("[\"already\"]"), "[\"already\"]");
    }

    #[test]
    fn test_config_json_detection() {
        let string_items = vec![ConfigItem {
            key: "key".to_owned(),
            v: Some(config_item::V::Value(prost_types::Value {
                kind: Some(prost_types::value::Kind::StringValue("val".to_owned())),
            })),
            path: None,
            secret: None,
        }];
        assert!(!has_non_string_values(&string_items));

        let number_items = vec![ConfigItem {
            key: "key".to_owned(),
            v: Some(config_item::V::Value(prost_types::Value {
                kind: Some(prost_types::value::Kind::NumberValue(42.0)),
            })),
            path: None,
            secret: None,
        }];
        assert!(has_non_string_values(&number_items));
    }
}
