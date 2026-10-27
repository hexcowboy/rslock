use rslock::LockManager;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    smol::block_on(async {
        let manager = LockManager::new(vec!["redis://127.0.0.1:6380/"]);
        let lock = manager
            .lock("smol-example", Duration::from_secs(10))
            .await?;
        manager.unlock(&lock).await;
        Ok(())
    })
}
