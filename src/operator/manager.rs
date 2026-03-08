use std::collections::HashSet;

use compact_str::CompactString;
use kube::runtime::reflector::Store;
use parking_lot::Mutex;

use crate::api::program::Program;
use crate::api::stack::Stack;
use crate::api::update::Update;
use crate::api::workspace::Workspace;
use crate::operator::connection::ConnectionPool;
use crate::operator::controllers::program::ProgramFileServer;
use crate::operator::events::EventRecorder;
use crate::operator::metrics::Metrics;

/// Shared informer caches -- populated by reflector-backed watchers.
/// Store<K> is internally Arc-backed (kube-rs internal, acceptable).
pub struct InformerStores {
    pub stacks: Store<Stack>,
    pub workspaces: Store<Workspace>,
    pub updates: Store<Update>,
    pub programs: Store<Program>,
}

/// Manager is the top-level operator state.
/// Leaked to `&'static` so actors can borrow without Arc.
pub struct Manager {
    pub client: kube::Client,
    pub pool: ConnectionPool,
    pub events: EventRecorder,
    pub metrics: Metrics,
    pub max_concurrent_reconciles: usize,
    pub stores: InformerStores,
    pub program_file_server: ProgramFileServer,
    /// Track which updates are currently being executed (survives stream reconnects).
    /// Using parking_lot::Mutex (not tokio) since Manager is &'static — no Arc needed.
    pub update_in_progress: Mutex<HashSet<CompactString>>,
}

impl Manager {
    pub fn new(
        client: kube::Client,
        max_concurrent_reconciles: usize,
        stores: InformerStores,
    ) -> Self {
        Self {
            pool: ConnectionPool::new(
                std::time::Duration::from_secs(7200), // 2 hour idle prune (matches Go)
            ),
            events: EventRecorder::new(client.clone()),
            metrics: Metrics::new(),
            client,
            max_concurrent_reconciles,
            stores,
            program_file_server: ProgramFileServer::new(),
            update_in_progress: Mutex::new(HashSet::new()),
        }
    }

    /// Return a static reference to the metrics, for use by background tasks.
    /// Only valid to call after `leak()`.
    pub fn metrics_ref(&'static self) -> &'static Metrics {
        &self.metrics
    }

    /// Leak to 'static so actors can borrow without Arc.
    /// The Manager lives for the entire process lifetime.
    pub fn leak(self) -> &'static Self {
        Box::leak(Box::new(self))
    }
}
