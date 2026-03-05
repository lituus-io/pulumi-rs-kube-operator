//! Minimal HTTP health/readiness server for Kubernetes probes.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

/// Shared health state, cheaply cloneable via Arc.
#[derive(Clone)]
pub struct HealthState {
    ready: Arc<AtomicBool>,
}

impl Default for HealthState {
    fn default() -> Self {
        Self::new()
    }
}

impl HealthState {
    pub fn new() -> Self {
        Self {
            ready: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Mark the operator as ready (call after watchers start).
    pub fn set_ready(&self) {
        self.ready.store(true, Ordering::Relaxed);
    }

    /// Mark the operator as not ready (call on shutdown).
    pub fn set_not_ready(&self) {
        self.ready.store(false, Ordering::Relaxed);
    }

    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }
}

async fn handle(
    req: Request<hyper::body::Incoming>,
    state: HealthState,
) -> Result<Response<Full<Bytes>>, Infallible> {
    match req.uri().path() {
        "/healthz" => Ok(Response::new(Full::from("ok"))),
        "/readyz" => {
            if state.is_ready() {
                Ok(Response::new(Full::from("ok")))
            } else {
                let mut resp = Response::new(Full::from("not ready"));
                *resp.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                Ok(resp)
            }
        }
        _ => {
            let mut resp = Response::new(Full::from("not found"));
            *resp.status_mut() = StatusCode::NOT_FOUND;
            Ok(resp)
        }
    }
}

/// Serve health endpoints on the given port. Runs until cancelled.
pub async fn serve(port: u16, state: HealthState) -> Result<(), crate::errors::RunError> {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "health server listening");

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = http1::Builder::new()
                .serve_connection(
                    io,
                    service_fn(move |req| {
                        let state = state.clone();
                        async move { handle(req, state).await }
                    }),
                )
                .await
            {
                tracing::debug!(error = %e, "health connection error");
            }
        });
    }
}
