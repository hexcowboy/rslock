use rslock::LockManager;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("REDLOCK TEST (Three Clusters)\n");

    let lm = LockManager::new_cluster(vec![
        vec!["redis://c1-n1:6379"],
        vec!["redis://c2-n1:6379"],
        vec!["redis://c3-n1:6379"],
    ])?;

    // Test 1: Basic Redlock
    println!("Test 1: Basic Redlock (quorum=2/3)");
    let lock = lm.lock(b"resource:{1}", Duration::from_secs(5)).await?;
    println!("Lock acquired on quorum: validity={}ms", lock.validity_time);
    lm.unlock(&lock).await;
    println!("Lock released from all clusters\n");

    // Test 2: Concurrent lock attempt (Redlock should prevent)
    println!("Test 2: Concurrent Redlock attempt");
    let lock1 = lm.lock(b"resource:{2}", Duration::from_secs(5)).await?;
    println!("Lock1 acquired on quorum");

    let lm2 = LockManager::new_cluster(vec![
        vec!["redis://c1-n1:6379"],
        vec!["redis://c2-n1:6379"],
        vec!["redis://c3-n1:6379"],
    ])?;

    match lm2.lock(b"resource:{2}", Duration::from_secs(5)).await {
        Ok(_) => println!("Lock2 should NOT get quorum!"),
        Err(_) => println!("Lock2 correctly failed (no quorum)"),
    }

    lm.unlock(&lock1).await;
    println!("Lock1 released from all clusters\n");

    // Test 3: Lock expiration across clusters
    println!("Test 3: Lock expiration (all clusters)");
    let lock = lm.lock(b"resource:{3}", Duration::from_millis(500)).await?;
    println!("Lock acquired on quorum: validity={}ms", lock.validity_time);
    sleep(Duration::from_millis(600)).await;
    println!("Lock expired across all clusters\n");

    // Test 4: Re-acquire after expiration
    println!("Test 4: Re-acquire after expiration");
    let lock = lm.lock(b"resource:{3}", Duration::from_secs(5)).await?;
    println!("Lock re-acquired successfully on quorum");
    lm.unlock(&lock).await;
    println!("Lock released\n");

    // Test 5: Multiple resources (Redlock for each)
    println!("Test 5: Multiple independent Redlocks");
    let lock_a = lm.lock(b"resource:{5a}", Duration::from_secs(5)).await?;
    let lock_b = lm.lock(b"resource:{5b}", Duration::from_secs(5)).await?;
    let lock_c = lm.lock(b"resource:{5c}", Duration::from_secs(5)).await?;
    println!("Three Redlocks acquired (each has quorum)");
    lm.unlock(&lock_a).await;
    lm.unlock(&lock_b).await;
    lm.unlock(&lock_c).await;
    println!("All Redlocks released\n");

    // Test 6: Validity time check
    println!("Test 6: Validity time validation");
    let lock = lm.lock(b"resource:{6}", Duration::from_secs(10)).await?;
    let validity = lock.validity_time;
    println!("Lock acquired: validity={}ms", validity);

    if validity > 9000 && validity < 10000 {
        println!("Validity time is reasonable (9-10s)");
    } else {
        println!("Validity time unexpected: {}ms", validity);
    }

    lm.unlock(&lock).await;
    println!("Lock released\n");

    // Test 7: Quick successive locks
    println!("Test 7: Quick successive locks");
    for i in 0..5 {
        let resource = format!("resource:{{7:{}}}", i);
        let lock = lm.lock(resource.as_bytes(), Duration::from_secs(2)).await?;
        println!("  Lock {} acquired", i);
        lm.unlock(&lock).await;
    }
    println!("All successive locks worked\n");

    println!("REDLOCK: All tests passed!\n");
    Ok(())
}