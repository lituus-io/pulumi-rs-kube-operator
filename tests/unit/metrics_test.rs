use std::sync::atomic::Ordering::Relaxed;

use pulumi_kubernetes_operator::operator::metrics::Metrics;

#[test]
fn new_initializes_all_counters_to_zero() {
    let m = Metrics::new();
    assert_eq!(m.reconciles_total.load(Relaxed), 0);
    assert_eq!(m.reconcile_errors.load(Relaxed), 0);
    assert_eq!(m.active_actors.load(Relaxed), 0);
    assert_eq!(m.lock_conflicts.load(Relaxed), 0);
    assert_eq!(m.force_unlocks.load(Relaxed), 0);
    assert_eq!(m.connection_pool_size.load(Relaxed), 0);
    assert_eq!(m.connection_pool_hits.load(Relaxed), 0);
    assert_eq!(m.connection_pool_misses.load(Relaxed), 0);
}

#[test]
fn inc_reconciles() {
    let m = Metrics::new();
    m.inc_reconciles();
    m.inc_reconciles();
    assert_eq!(m.reconciles_total.load(Relaxed), 2);
}

#[test]
fn inc_reconcile_errors() {
    let m = Metrics::new();
    m.inc_reconcile_errors();
    assert_eq!(m.reconcile_errors.load(Relaxed), 1);
}

#[test]
fn inc_dec_active_actors() {
    let m = Metrics::new();
    m.inc_active_actors();
    m.inc_active_actors();
    m.inc_active_actors();
    assert_eq!(m.active_actors.load(Relaxed), 3);
    m.dec_active_actors();
    assert_eq!(m.active_actors.load(Relaxed), 2);
}

#[test]
fn inc_lock_conflicts() {
    let m = Metrics::new();
    m.inc_lock_conflicts();
    m.inc_lock_conflicts();
    assert_eq!(m.lock_conflicts.load(Relaxed), 2);
}

#[test]
fn inc_force_unlocks() {
    let m = Metrics::new();
    m.inc_force_unlocks();
    assert_eq!(m.force_unlocks.load(Relaxed), 1);
}

#[test]
fn set_pool_size() {
    let m = Metrics::new();
    m.set_pool_size(42);
    assert_eq!(m.connection_pool_size.load(Relaxed), 42);
    m.set_pool_size(0);
    assert_eq!(m.connection_pool_size.load(Relaxed), 0);
}

#[test]
fn inc_pool_hits_and_misses() {
    let m = Metrics::new();
    m.inc_pool_hits();
    m.inc_pool_hits();
    m.inc_pool_misses();
    assert_eq!(m.connection_pool_hits.load(Relaxed), 2);
    assert_eq!(m.connection_pool_misses.load(Relaxed), 1);
}

#[test]
fn encode_returns_valid_prometheus_text() {
    let m = Metrics::new();
    m.inc_reconciles();
    m.inc_reconcile_errors();
    let text = m.encode();
    assert!(text.contains("pulumi_operator_reconciles_total"));
    assert!(text.contains("pulumi_operator_reconcile_errors_total"));
    assert!(text.contains("pulumi_operator_active_actors"));
    assert!(text.contains("pulumi_operator_lock_conflicts_total"));
    assert!(text.contains("pulumi_operator_force_unlocks_total"));
}

#[test]
fn default_same_as_new() {
    let d = Metrics::default();
    let n = Metrics::new();
    assert_eq!(d.reconciles_total.load(Relaxed), n.reconciles_total.load(Relaxed));
    assert_eq!(d.reconcile_errors.load(Relaxed), n.reconcile_errors.load(Relaxed));
    assert_eq!(d.active_actors.load(Relaxed), n.active_actors.load(Relaxed));
}

#[test]
fn encode_empty_is_nonempty() {
    let m = Metrics::new();
    let text = m.encode();
    // Even with zero values, prometheus-client emits metric descriptors
    assert!(!text.is_empty());
}
