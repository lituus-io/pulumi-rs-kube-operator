use clap::{Parser, Subcommand};
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Parser)]
#[command(name = "pulumi-kubernetes-operator")]
#[command(about = "Pulumi Kubernetes Operator")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the operator control plane
    Operator {
        /// Maximum concurrent reconciles
        #[arg(long, default_value_t = 25)]
        max_concurrent_reconciles: usize,

        /// Enable leader election
        #[arg(long)]
        leader_elect: bool,

        /// Metrics bind address
        #[arg(long, default_value = ":8080")]
        metrics_bind_address: String,

        /// Health probe bind address
        #[arg(long, default_value = ":8081")]
        health_probe_bind_address: String,
    },
    /// Run the agent gRPC server inside a workspace pod
    Agent {
        /// gRPC listen address
        #[arg(long, default_value = "0.0.0.0:50051")]
        listen_address: String,

        /// Workspace directory
        #[arg(long, default_value = "/share")]
        workspace_dir: String,

        /// Log verbosity for Pulumi operations
        #[arg(long, default_value_t = 0)]
        log_level: u32,
    },
    /// Initialize a workspace (run as init container)
    Init {
        /// Source directory (where source code is fetched to)
        #[arg(long, default_value = "/share/source")]
        source_dir: String,

        /// Workspace directory (symlinked to source_dir/subdir)
        #[arg(long, default_value = "/share/workspace")]
        workspace_dir: String,
    },
    /// Run the GitHub webhook server for PR preview
    Webhook {
        /// Webhook listen port
        #[arg(long, default_value_t = 8090)]
        port: u16,

        /// GitHub webhook secret (for HMAC validation)
        #[arg(long, env = "GITHUB_WEBHOOK_SECRET")]
        secret: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<(), pulumi_kubernetes_operator::errors::RunError> {
    // Initialize tracing
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .json()
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Operator {
            max_concurrent_reconciles,
            leader_elect,
            metrics_bind_address,
            health_probe_bind_address,
        } => {
            tracing::info!(
                max_concurrent_reconciles,
                leader_elect,
                %metrics_bind_address,
                %health_probe_bind_address,
                "starting operator"
            );
            pulumi_kubernetes_operator::operator::run(
                max_concurrent_reconciles,
                leader_elect,
            )
            .await?;
        }
        Command::Agent {
            listen_address,
            workspace_dir,
            log_level,
        } => {
            tracing::info!(%listen_address, %workspace_dir, log_level, "starting agent");
            pulumi_kubernetes_operator::agent::serve(&listen_address, &workspace_dir, log_level)
                .await?;
        }
        Command::Init { source_dir, workspace_dir } => {
            tracing::info!(%workspace_dir, %source_dir, "initializing workspace");
            pulumi_kubernetes_operator::agent::init(&workspace_dir, &source_dir).await?;
        }
        Command::Webhook { port, secret } => {
            tracing::info!(port, has_secret = secret.is_some(), "starting webhook server");
            pulumi_kubernetes_operator::operator::webhook::serve_webhook(port, secret)
                .await?;
        }
    }

    Ok(())
}
