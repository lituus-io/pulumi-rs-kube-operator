pub mod actors;
pub mod connection;
pub mod controllers;
pub mod finalizers;
pub mod events;
pub mod health;
pub mod lock;
pub mod manager;
pub mod metrics;
pub mod reconcile;
pub mod shutdown;
pub mod status;
pub mod webhook;


/// Entry point for the operator control plane.
pub async fn run(
    max_concurrent_reconciles: usize,
    leader_elect: bool,
) -> Result<(), crate::errors::RunError> {
    let client = kube::Client::try_default().await?;

    // Create reflector stores before Manager so caches are ready
    let (stores, stack_writer, ws_writer, update_writer, program_writer) =
        controllers::create_stores();

    let mgr = manager::Manager::new(client, max_concurrent_reconciles, stores);
    let mgr: &'static manager::Manager = mgr.leak();

    tracing::info!(
        max_concurrent_reconciles,
        leader_elect,
        "operator manager initialized"
    );

    let dispatcher = actors::dispatcher::Dispatcher::new(mgr);

    // Set up graceful shutdown signal
    let shutdown_rx = shutdown::shutdown_signal();

    // Start health/readiness server in background
    let health_state = health::HealthState::new();
    let health_state_clone = health_state.clone();
    tokio::spawn(health::serve(8081, health_state.clone()));

    // Start metrics server in background (port 8080)
    tokio::spawn(metrics::serve_metrics(mgr.metrics_ref(), 8080));

    // Mark ready once watchers start (set inside run_all via the HealthState)
    health_state_clone.set_ready();

    // Pool evictor runs inside controllers::run_all (run_pool_evictor)

    // Run controllers with graceful shutdown
    let result = tokio::select! {
        result = controllers::run_all(
            mgr,
            dispatcher,
            stack_writer,
            ws_writer,
            update_writer,
            program_writer,
        ) => {
            result
        }
        _ = shutdown::wait_for_shutdown(shutdown_rx) => {
            tracing::info!("graceful shutdown initiated, draining actors...");
            Ok(())
        }
    };

    health_state_clone.set_not_ready();
    result
}
