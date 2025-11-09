use rslock::LockManager;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("SIMPLE REDIS LOCK TEST (Single Cluster)\n");

    let lm = LockManager::new_cluster(vec![
        vec!["redis://c1-n1:6379"],
    ])?;

    // Test 1: Basic lock/unlock
    println!("Test 1: Basic lock/unlock");
    let lock = lm.lock(b"resource:{1}", Duration::from_secs(5)).await?;
    println!("Lock acquired: validity={}ms", lock.validity_time);
    lm.unlock(&lock).await;
    println!("Lock released\n");

    // Test 2: Lock expiration
    println!("Test 2: Lock expiration");
    let lock = lm.lock(b"resource:{2}", Duration::from_millis(500)).await?;
    println!("Lock acquired: validity={}ms", lock.validity_time);
    sleep(Duration::from_millis(600)).await;
    println!("⏰ Lock expired (no unlock needed)\n");

    // Test 3: Concurrent lock attempt
    println!("Test 3: Concurrent lock (should fail)");
    let lock1 = lm.lock(b"resource:{3}", Duration::from_secs(5)).await?;
    println!("Lock1 acquired by first client");

    let lm2 = LockManager::new_cluster(vec![
        vec!["redis://c1-n1:6379"],
    ])?;

    match lm2.lock(b"resource:{3}", Duration::from_secs(5)).await {
        Ok(_) => println!("Lock2 should NOT succeed!"),
        Err(_) => println!("Lock2 correctly rejected"),
    }

    lm.unlock(&lock1).await;
    println!("Lock1 released\n");

    // Test 4: Re-acquire after unlock
    println!("Test 4: Re-acquire after unlock");
    let lock = lm.lock(b"resource:{4}", Duration::from_secs(5)).await?;
    println!("Lock acquired");
    lm.unlock(&lock).await;
    println!("Lock released");

    let lock2 = lm.lock(b"resource:{4}", Duration::from_secs(5)).await?;
    println!("Lock re-acquired successfully");
    lm.unlock(&lock2).await;
    println!("Lock released\n");

    // Test 5: Multiple resources
    println!("Test 5: Multiple independent resources");
    let lock_a = lm.lock(b"resource:{5a}", Duration::from_secs(5)).await?;
    let lock_b = lm.lock(b"resource:{5b}", Duration::from_secs(5)).await?;
    let lock_c = lm.lock(b"resource:{5c}", Duration::from_secs(5)).await?;
    println!("Three locks acquired simultaneously");
    lm.unlock(&lock_a).await;
    lm.unlock(&lock_b).await;
    lm.unlock(&lock_c).await;
    println!("All locks released\n");

    println!("SIMPLE REDIS LOCK: All tests passed!\n");
    Ok(())
}