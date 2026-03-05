use pulumi_kubernetes_operator::operator::metrics::Metrics;

use std::sync::atomic::Ordering::Relaxed;

#[test]
fn manager_default_metrics_are_zero() {
    let metrics = Metrics::new();
    assert_eq!(metrics.reconciles_total.load(Relaxed), 0);
    assert_eq!(metrics.reconcile_errors.load(Relaxed), 0);
    assert_eq!(metrics.active_actors.load(Relaxed), 0);
    assert_eq!(metrics.lock_conflicts.load(Relaxed), 0);
    assert_eq!(metrics.force_unlocks.load(Relaxed), 0);
    assert_eq!(metrics.connection_pool_size.load(Relaxed), 0);
    assert_eq!(metrics.connection_pool_hits.load(Relaxed), 0);
    assert_eq!(metrics.connection_pool_misses.load(Relaxed), 0);
}

#[test]
fn manager_metrics_increment_correctly() {
    let metrics = Metrics::new();

    metrics.inc_reconciles();
    metrics.inc_reconciles();
    assert_eq!(metrics.reconciles_total.load(Relaxed), 2);

    metrics.inc_reconcile_errors();
    assert_eq!(metrics.reconcile_errors.load(Relaxed), 1);

    metrics.inc_active_actors();
    metrics.inc_active_actors();
    assert_eq!(metrics.active_actors.load(Relaxed), 2);

    metrics.dec_active_actors();
    assert_eq!(metrics.active_actors.load(Relaxed), 1);
}

#[test]
fn manager_pool_metrics_track_hits_misses() {
    let metrics = Metrics::new();

    metrics.set_pool_size(5);
    assert_eq!(metrics.connection_pool_size.load(Relaxed), 5);

    metrics.inc_pool_hits();
    metrics.inc_pool_hits();
    metrics.inc_pool_misses();
    assert_eq!(metrics.connection_pool_hits.load(Relaxed), 2);
    assert_eq!(metrics.connection_pool_misses.load(Relaxed), 1);
}

#[test]
fn manager_metrics_encode_produces_valid_prometheus() {
    let metrics = Metrics::new();
    metrics.inc_reconciles();

    let encoded = metrics.encode();
    assert!(!encoded.is_empty());
    assert!(encoded.contains("pulumi_operator_reconciles_total"));
    assert!(encoded.contains("pulumi_operator_reconcile_errors_total"));
    assert!(encoded.contains("pulumi_operator_active_actors"));
}

#[test]
fn manager_program_file_server_is_empty() {
    use pulumi_kubernetes_operator::operator::controllers::program::ProgramFileServer;

    let server = ProgramFileServer::new();
    assert_eq!(server.get_artifact("any-path"), None);
}
