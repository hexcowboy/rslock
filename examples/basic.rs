use std::time::Duration;

use rslock::LockManager;

#[tokio::main]
async fn main() {
    let rl = LockManager::new(vec![
        "redis://127.0.0.1:6380/",
        "redis://127.0.0.1:6381/",
        "redis://127.0.0.1:6382/",
    ]);

    let lock;
    loop {
        // Create the lock
        if let Ok(l) = rl
            .lock("mutex".as_bytes(), Duration::from_millis(1000))
            .await
        {
            lock = l;
            break;
        }
    }

    // Extend the lock
    match rl.extend(&lock, Duration::from_millis(1000)).await {
        Ok(_) => println!("lock extended!"),
        Err(_) => println!("lock couldn't be extended"),
    }

    // Unlock the lock
    rl.unlock(&lock).await;
}
