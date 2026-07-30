use rslock::LockManager;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // These are seed nodes for one logical Redis Cluster, not independent Redlock servers.
    // The cluster client discovers the rest of the topology automatically.
    let manager = LockManager::new_cluster(vec![
        "redis://127.0.0.1:7000/",
        "redis://127.0.0.1:7001/",
        "redis://127.0.0.1:7002/",
    ])?;

    let lock = manager
        .lock("cluster-example", Duration::from_secs(10))
        .await?;

    println!("Lock acquired");
    manager.unlock(&lock).await;
    println!("Lock released");

    Ok(())
}
