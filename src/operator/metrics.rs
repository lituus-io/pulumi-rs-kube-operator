use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use prometheus_client::encoding::text::encode;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::registry::Registry;
use tokio::net::TcpListener;

/// Lock-free atomic metrics counters + prometheus-client registry.
pub struct Metrics {
    // Atomic counters for fast, lock-free updates
    pub reconciles_total: AtomicU64,
    pub reconcile_errors: AtomicU64,
    pub active_actors: AtomicU64,
    pub lock_conflicts: AtomicU64,
    pub force_unlocks: AtomicU64,
    pub connection_pool_size: AtomicU64,
    pub connection_pool_hits: AtomicU64,
    pub connection_pool_misses: AtomicU64,
    pub mailbox_drops: AtomicU64,

    // Prometheus registry metrics (synced from atomics on scrape)
    registry: parking_lot::RwLock<Registry>,
    prom_reconciles: Counter,
    prom_errors: Counter,
    prom_actors: Gauge,
    prom_lock_conflicts: Counter,
    prom_force_unlocks: Counter,
    prom_mailbox_drops: Counter,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub fn new() -> Self {
        let mut registry = Registry::default();

        let reconciles = Counter::default();
        let errors = Counter::default();
        let actors = Gauge::default();
        let lock_conflicts = Counter::default();
        let force_unlocks = Counter::default();
        let mailbox_drops = Counter::default();

        registry.register(
            "pulumi_operator_reconciles_total",
            "Total number of reconciliation attempts",
            reconciles.clone(),
        );
        registry.register(
            "pulumi_operator_reconcile_errors_total",
            "Total number of reconciliation errors",
            errors.clone(),
        );
        registry.register(
            "pulumi_operator_active_actors",
            "Number of active actor goroutines",
            actors.clone(),
        );
        registry.register(
            "pulumi_operator_lock_conflicts_total",
            "Total number of update lock conflicts",
            lock_conflicts.clone(),
        );
        registry.register(
            "pulumi_operator_force_unlocks_total",
            "Total number of force unlock operations",
            force_unlocks.clone(),
        );
        registry.register(
            "pulumi_operator_mailbox_drops_total",
            "Total number of messages dropped due to full actor mailbox",
            mailbox_drops.clone(),
        );

        Self {
            reconciles_total: AtomicU64::new(0),
            reconcile_errors: AtomicU64::new(0),
            active_actors: AtomicU64::new(0),
            lock_conflicts: AtomicU64::new(0),
            force_unlocks: AtomicU64::new(0),
            connection_pool_size: AtomicU64::new(0),
            connection_pool_hits: AtomicU64::new(0),
            connection_pool_misses: AtomicU64::new(0),
            mailbox_drops: AtomicU64::new(0),
            registry: parking_lot::RwLock::new(registry),
            prom_reconciles: reconciles,
            prom_errors: errors,
            prom_actors: actors,
            prom_lock_conflicts: lock_conflicts,
            prom_force_unlocks: force_unlocks,
            prom_mailbox_drops: mailbox_drops,
        }
    }

    pub fn inc_reconciles(&self) {
        self.reconciles_total.fetch_add(1, Relaxed);
        self.prom_reconciles.inc();
    }

    pub fn inc_reconcile_errors(&self) {
        self.reconcile_errors.fetch_add(1, Relaxed);
        self.prom_errors.inc();
    }

    pub fn inc_active_actors(&self) {
        self.active_actors.fetch_add(1, Relaxed);
        self.prom_actors.inc();
    }

    pub fn dec_active_actors(&self) {
        self.active_actors.fetch_sub(1, Relaxed);
        self.prom_actors.dec();
    }

    pub fn inc_lock_conflicts(&self) {
        self.lock_conflicts.fetch_add(1, Relaxed);
        self.prom_lock_conflicts.inc();
    }

    pub fn inc_force_unlocks(&self) {
        self.force_unlocks.fetch_add(1, Relaxed);
        self.prom_force_unlocks.inc();
    }

    pub fn set_pool_size(&self, size: u64) {
        self.connection_pool_size.store(size, Relaxed);
    }

    pub fn inc_pool_hits(&self) {
        self.connection_pool_hits.fetch_add(1, Relaxed);
    }

    pub fn inc_pool_misses(&self) {
        self.connection_pool_misses.fetch_add(1, Relaxed);
    }

    pub fn inc_mailbox_drops(&self) {
        self.mailbox_drops.fetch_add(1, Relaxed);
        self.prom_mailbox_drops.inc();
    }

    /// Encode all metrics in Prometheus text format.
    pub fn encode(&self) -> String {
        let registry = self.registry.read();
        let mut buf = String::with_capacity(4096);
        encode(&mut buf, &registry).unwrap_or_default();
        buf
    }
}

/// Serve metrics endpoint on the given port. Runs until cancelled.
pub async fn serve_metrics(
    metrics: &'static Metrics,
    port: u16,
) -> Result<(), crate::errors::RunError> {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "metrics server listening");

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        tokio::spawn(async move {
            if let Err(e) = http1::Builder::new()
                .serve_connection(
                    io,
                    service_fn(move |req| handle_metrics(req, metrics)),
                )
                .await
            {
                tracing::debug!(error = %e, "metrics connection error");
            }
        });
    }
}

async fn handle_metrics(
    req: Request<hyper::body::Incoming>,
    metrics: &'static Metrics,
) -> Result<Response<Full<Bytes>>, Infallible> {
    match req.uri().path() {
        "/metrics" => {
            let body = metrics.encode();
            Ok(Response::new(Full::from(body)))
        }
        _ => {
            let mut resp = Response::new(Full::from("not found"));
            *resp.status_mut() = StatusCode::NOT_FOUND;
            Ok(resp)
        }
    }
}
