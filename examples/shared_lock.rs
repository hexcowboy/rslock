use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task;

use rslock::{Lock, LockManager};

// This example demonstrates how to use a shared lock in a multi-threaded environment.

#[tokio::main]
async fn main() {
    // Create a shared LockManager with multiple Redis instances
    let rl = Arc::new(LockManager::new(vec![
        "redis://127.0.0.1:6380/",
        "redis://127.0.0.1:6381/",
        "redis://127.0.0.1:6382/",
    ]));

    // Create a channel to communicate between threads
    let (tx, mut rx) = mpsc::channel::<Arc<Lock>>(1);

    // Spawn a task to acquire a lock and send it to another task
    let rl_clone = rl.clone();
    let sender_task = task::spawn(async move {
        let lock;
        loop {
            // Attempt to acquire the lock
            if let Ok(l) = rl_clone
                .lock("shared_mutex".as_bytes(), Duration::from_millis(2000))
                .await
            {
                lock = Arc::new(l);
                break;
            }
        }
        println!("Lock acquired by sender task.");

        // Send the lock to the receiver task
        tx.send(lock.clone()).await.unwrap();

        // Extend the lock
        match rl_clone.extend(&lock, Duration::from_millis(2000)).await {
            Ok(_) => println!("Lock extended by sender task!"),
            Err(_) => println!("Sender task couldn't extend the lock"),
        }
    });

    // Spawn a task to receive the lock and release it
    let receiver_task = task::spawn(async move {
        if let Some(lock) = rx.recv().await {
            println!("Lock received by receiver task.");

            // Extend the lock
            match lock
                .lock_manager
                .extend(&lock, Duration::from_millis(1000))
                .await
            {
                Ok(_) => println!("Lock extended by receiver task!"),
                Err(_) => println!("Receiver task couldn't extend the lock"),
            }

            // Unlock the lock
            lock.lock_manager.unlock(&lock).await;
            println!("Lock released by receiver task.");
        }
    });

    // Wait for both tasks to complete
    let _ = tokio::join!(sender_task, receiver_task);
}
