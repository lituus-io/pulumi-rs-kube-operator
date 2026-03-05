use pulumi_kubernetes_operator::operator::shutdown::wait_for_shutdown;
use tokio::sync::watch;

#[tokio::test]
async fn returns_immediately_when_already_true() {
    let (_tx, rx) = watch::channel(true);
    // Should complete immediately since value is already true
    wait_for_shutdown(rx).await;
}

#[tokio::test]
async fn blocks_until_signaled() {
    let (tx, rx) = watch::channel(false);

    let handle = tokio::spawn(async move {
        wait_for_shutdown(rx).await;
    });

    // Signal shutdown
    tx.send(true).unwrap();
    // Should complete now
    tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("timed out waiting for shutdown")
        .expect("task panicked");
}

#[tokio::test]
async fn returns_when_sender_dropped() {
    let (tx, rx) = watch::channel(false);

    let handle = tokio::spawn(async move {
        wait_for_shutdown(rx).await;
    });

    // Drop the sender — rx.changed() will return Err, breaking the loop
    drop(tx);
    tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("timed out waiting for shutdown")
        .expect("task panicked");
}
