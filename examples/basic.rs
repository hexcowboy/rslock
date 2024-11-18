use rslock::LockManager;
use std::time::Duration;

#[tokio::main]
async fn main() {
    // Define Redis URIs
    let uris = vec![
        "redis://127.0.0.1:6380/",
        "redis://127.0.0.1:6381/",
        "redis://127.0.0.1:6382/",
    ];

    // Initialize the LockManager using `new`
    let rl = LockManager::new(uris);

    // Acquire a lock
    let lock = loop {
        if let Ok(lock) = rl
            .lock("mutex".as_bytes(), Duration::from_millis(1000))
            .await
        {
            break lock;
        }
    };

    println!("Lock acquired!");

    // Extend the lock
    if rl.extend(&lock, Duration::from_millis(1000)).await.is_ok() {
        println!("Lock extended!");
    } else {
        println!("Failed to extend the lock.");
    }

    // Unlock the lock
    rl.unlock(&lock).await;
    println!("Lock released!");
}
