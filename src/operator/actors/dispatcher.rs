use std::collections::HashMap;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::mpsc;

use crate::operator::manager::Manager;

use super::actor::Actor;
use super::messages::{NameKey, PrioritizedMessage, Priority, StackMessage};

struct ActorHandle {
    tx: mpsc::Sender<PrioritizedMessage>,
    handle: tokio::task::JoinHandle<()>,
}

pub struct Dispatcher {
    mgr: &'static Manager,
    actors: Mutex<HashMap<NameKey, ActorHandle>>,
}

impl Dispatcher {
    pub fn new(mgr: &'static Manager) -> Self {
        Self {
            mgr,
            actors: Mutex::new(HashMap::new()),
        }
    }

    /// Route an event to the correct actor, creating one if needed.
    /// No dynamic dispatch -- the actor task is a concrete async fn.
    pub async fn dispatch(&self, key: NameKey, msg: StackMessage) {
        self.dispatch_with_priority(key, Priority::Normal, msg)
            .await;
    }

    pub async fn dispatch_with_priority(
        &self,
        key: NameKey,
        priority: Priority,
        msg: StackMessage,
    ) {
        let tx = {
            let mut actors = self.actors.lock();

            // Clean up finished actors
            actors.retain(|_, handle| !handle.handle.is_finished());

            let handle = actors.entry(key.clone()).or_insert_with(|| {
                let (tx, rx) = mpsc::channel(64);
                let actor = Actor::new(self.mgr, key.clone(), rx, tx.clone());
                let jh = tokio::spawn(actor.run());
                ActorHandle { tx, handle: jh }
            });

            handle.tx.clone()
        };

        let pmsg = PrioritizedMessage {
            priority,
            inner: msg,
        };

        // Use try_send to avoid blocking the watcher loop when mailbox is full.
        // If full, the actor already has work queued and will reconcile.
        match tx.try_send(pmsg) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(key = %key, "actor mailbox full, message dropped");
                self.mgr.metrics.inc_mailbox_drops();
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::warn!(key = %key, "actor mailbox closed, message dropped");
            }
        }
    }

    /// Shutdown all actors gracefully.
    /// Drops all senders so `rx.recv()` returns None, causing actors to exit naturally.
    /// Times out after 5 seconds per actor to prevent hanging.
    pub async fn shutdown_all(&self) {
        let handles: Vec<_> = {
            let mut actors = self.actors.lock();
            actors
                .drain()
                .map(|(key, handle)| {
                    // Drop the sender — recv() returns None, actor exits loop
                    drop(handle.tx);
                    (key, handle.handle)
                })
                .collect()
        };

        for (key, handle) in handles {
            match tokio::time::timeout(Duration::from_secs(5), handle).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!(%key, error = %e, "actor task panicked during shutdown");
                }
                Err(_) => {
                    tracing::warn!(%key, "actor shutdown timed out after 5s");
                }
            }
        }
    }

    /// Get the number of active actors.
    pub fn active_count(&self) -> usize {
        self.actors.lock().len()
    }
}
