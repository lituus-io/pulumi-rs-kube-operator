pub mod auth;
pub mod cancel;
pub mod init;
pub mod redact;
pub mod server;

/// Start the agent gRPC server.
pub async fn serve(
    listen_address: &str,
    workspace_dir: &str,
    log_level: u32,
) -> Result<(), crate::errors::RunError> {
    server::run_server(listen_address, workspace_dir, log_level).await
}

/// Initialize a workspace (run as init container).
/// `source_dir` is where source code is fetched to (e.g., `/share/source`).
/// `workspace_dir` is symlinked to `source_dir/{subdir}` (e.g., `/share/workspace`).
pub async fn init(workspace_dir: &str, source_dir: &str) -> Result<(), crate::errors::RunError> {
    Ok(init::initialize_workspace(workspace_dir, source_dir).await?)
}
