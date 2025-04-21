use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task;

use rslock::{Lock, LockManager};

// Demonstrates using a shared lock in a multi-threaded environment.
#[tokio::main]
async fn main() {
    // Create a shared LockManager with multiple Redis instances
    let uris = vec![
        "redis://127.0.0.1:6380/",
        "redis://127.0.0.1:6381/",
        "redis://127.0.0.1:6382/",
    ];
    let lock_manager = Arc::new(LockManager::new(uris));

    // Create a channel to communicate between tasks
    let (tx, mut rx) = mpsc::channel::<Arc<Lock>>(1);

    // Task to acquire a lock and send it to the receiver task
    let sender = {
        let lock_manager = Arc::clone(&lock_manager);
        task::spawn(async move {
            // Acquire the lock
            let lock = loop {
                match lock_manager
                    .lock("shared_mutex", Duration::from_millis(2000))
                    .await
                {
                    Ok(lock) => break Arc::new(lock),
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await, // Retry after a short delay
                }
            };

            println!("Sender: Lock acquired.");

            // Send the lock to the receiver
            if tx.send(lock.clone()).await.is_err() {
                println!("Sender: Failed to send the lock.");
                return;
            }

            println!("Sender: Lock sent.");

            // Extend the lock
            if lock_manager
                .extend(&lock, Duration::from_millis(2000))
                .await
                .is_ok()
            {
                println!("Sender: Lock extended.");
            } else {
                println!("Sender: Failed to extend the lock.");
            }
        })
    };

    // Task to receive the lock and release it
    let receiver = task::spawn(async move {
        if let Some(lock) = rx.recv().await {
            println!("Receiver: Lock received.");

            // Extend the lock
            if lock
                .lock_manager
                .extend(&lock, Duration::from_millis(1000))
                .await
                .is_ok()
            {
                println!("Receiver: Lock extended.");
            } else {
                println!("Receiver: Failed to extend the lock.");
            }

            // Release the lock
            lock.lock_manager.unlock(&lock).await;
            println!("Receiver: Lock released.");
        } else {
            println!("Receiver: No lock received.");
        }
    });

    // Wait for both tasks to complete
    let _ = tokio::join!(sender, receiver);
}
