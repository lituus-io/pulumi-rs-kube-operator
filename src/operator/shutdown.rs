//! Graceful shutdown handler.
//! Listens for SIGTERM and SIGINT, then drains actors and stops watchers.

use tokio::sync::watch;

/// Returns a receiver that is signaled on SIGTERM/SIGINT.
/// All watchers and actors should select on the receiver to stop gracefully.
pub fn shutdown_signal() -> watch::Receiver<bool> {
    let (tx, rx) = watch::channel(false);

    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();

        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm =
                signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");

            tokio::select! {
                _ = ctrl_c => {
                    tracing::info!("received SIGINT, initiating graceful shutdown");
                }
                _ = sigterm.recv() => {
                    tracing::info!("received SIGTERM, initiating graceful shutdown");
                }
            }
        }

        #[cfg(not(unix))]
        {
            ctrl_c.await.expect("failed to listen for ctrl-c");
            tracing::info!("received SIGINT, initiating graceful shutdown");
        }

        let _ = tx.send(true);
    });

    rx
}

/// Wait for the shutdown signal on the receiver.
pub async fn wait_for_shutdown(mut rx: watch::Receiver<bool>) {
    while !*rx.borrow() {
        if rx.changed().await.is_err() {
            break;
        }
    }
}
