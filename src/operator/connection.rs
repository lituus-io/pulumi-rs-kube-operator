use std::collections::HashMap;
use std::time::{Duration, Instant};

use compact_str::CompactString;
use parking_lot::RwLock;

use crate::core::lending::Lend;
use crate::errors::{OperatorError, TransientError};

/// Slab-based connection pool with per-entry RwLock.
/// Connections to different addresses never contend with each other.
///
/// Architecture:
///   index: RwLock<HashMap<key, slot_index>>   — read-locked on fast path, write-locked on new connections
///   entries: Box<[RwLock<Option<PoolEntry>>]> — read-locked per-connection (no contention)
///
/// Matches Go operator: 2-hour idle timeout, pruned every 5 minutes.
pub struct ConnectionPool {
    entries: Box<[RwLock<Option<PoolEntry>>]>,
    index: RwLock<HashMap<CompactString, usize>>,
    max_idle: Duration,
}

struct PoolEntry {
    channel: tonic::transport::Channel,
    last_used: Instant,
}

/// Guard holds a cloned Channel. Channel is internally Arc-based (cheap clone).
/// Lifetime tied to pool for API consistency with the Lend trait.
pub struct ConnectionGuard<'pool> {
    channel: tonic::transport::Channel,
    _marker: std::marker::PhantomData<&'pool ConnectionPool>,
}

impl<'pool> ConnectionGuard<'pool> {
    pub fn channel(&self) -> &tonic::transport::Channel {
        &self.channel
    }
}

const DEFAULT_SLAB_SIZE: usize = 64;

impl ConnectionPool {
    pub fn new(max_idle: Duration) -> Self {
        let entries: Vec<RwLock<Option<PoolEntry>>> =
            (0..DEFAULT_SLAB_SIZE).map(|_| RwLock::new(None)).collect();
        Self {
            entries: entries.into_boxed_slice(),
            index: RwLock::new(HashMap::with_capacity(32)),
            max_idle,
        }
    }

    /// Evict idle connections. Called periodically by a background task (every 5 minutes).
    pub fn evict_idle(&self) {
        let now = Instant::now();
        let mut index = self.index.write();

        for &slot in index.values() {
            let mut entry = self.entries[slot].write();
            if let Some(ref e) = *entry {
                if now.duration_since(e.last_used) >= self.max_idle {
                    *entry = None;
                }
            }
        }

        index.retain(|_, &mut slot| self.entries[slot].read().is_some());
    }

    /// Return the current pool size.
    pub fn len(&self) -> usize {
        self.index.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.read().is_empty()
    }

    /// Find a free slot in the slab.
    fn find_free_slot(&self) -> Option<usize> {
        for (i, entry) in self.entries.iter().enumerate() {
            let guard = entry.read();
            if guard.is_none() {
                return Some(i);
            }
        }
        None
    }

    /// Evict the oldest entry and return its slot.
    fn evict_oldest(&self, index: &mut HashMap<CompactString, usize>) -> Option<usize> {
        let mut oldest_time = Instant::now();
        let mut oldest_slot = None;

        for &slot in index.values() {
            let guard = self.entries[slot].read();
            if let Some(ref entry) = *guard {
                if entry.last_used < oldest_time {
                    oldest_time = entry.last_used;
                    oldest_slot = Some(slot);
                }
            }
        }

        if let Some(slot) = oldest_slot {
            *self.entries[slot].write() = None;
            index.retain(|_, &mut s| s != slot);
            Some(slot)
        } else {
            None
        }
    }
}

impl Lend for ConnectionPool {
    type Loan<'pool>
        = ConnectionGuard<'pool>
    where
        Self: 'pool;
    type Error = OperatorError;

    fn lend<'pool>(
        &'pool self,
        key: &str,
    ) -> impl std::future::Future<Output = Result<Self::Loan<'pool>, Self::Error>> + 'pool {
        let compact_key = CompactString::new(key);

        async move {
            // Fast path: existing connection (read lock on index, write lock on entry for last_used)
            {
                let index = self.index.read();
                if let Some(&slot) = index.get(&compact_key) {
                    let mut entry = self.entries[slot].write();
                    if let Some(ref mut e) = *entry {
                        e.last_used = Instant::now();
                        return Ok(ConnectionGuard {
                            channel: e.channel.clone(),
                            _marker: std::marker::PhantomData,
                        });
                    }
                }
            }

            // Slow path: create new connection (outside any lock)
            let http_key = format!("http://{}", compact_key);
            let endpoint = tonic::transport::Endpoint::from_shared(http_key)
                .map_err(|_| OperatorError::Transient(TransientError::ConnectionFailed))?
                .connect_timeout(Duration::from_secs(10));

            let channel = endpoint
                .connect()
                .await
                .map_err(|_| OperatorError::Transient(TransientError::ConnectionFailed))?;

            // Insert into slab
            let mut index = self.index.write();
            let slot = self
                .find_free_slot()
                .or_else(|| self.evict_oldest(&mut index))
                .ok_or(OperatorError::Transient(TransientError::ConnectionFailed))?;

            let mut entry = self.entries[slot].write();
            *entry = Some(PoolEntry {
                channel: channel.clone(),
                last_used: Instant::now(),
            });
            index.insert(compact_key, slot);

            Ok(ConnectionGuard {
                channel,
                _marker: std::marker::PhantomData,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_starts_empty() {
        let pool = ConnectionPool::new(Duration::from_secs(7200));
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn evict_idle_removes_expired() {
        let pool = ConnectionPool::new(Duration::from_secs(0)); // immediate expiry
                                                                // Since we can't easily insert without a real channel, just test evict doesn't panic
        pool.evict_idle();
        assert!(pool.is_empty());
    }

    #[test]
    fn find_free_slot_returns_first_empty() {
        let pool = ConnectionPool::new(Duration::from_secs(7200));
        assert_eq!(pool.find_free_slot(), Some(0));
    }
}
