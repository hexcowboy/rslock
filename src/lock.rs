use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::join_all;
use futures::Future;
use rand::{thread_rng, Rng, RngCore};
use redis::Value::Okay;
use redis::{Client, IntoConnectionInfo, RedisResult, Value};

const DEFAULT_RETRY_COUNT: u32 = 3;
const DEFAULT_RETRY_DELAY: Duration = Duration::from_millis(200);
const CLOCK_DRIFT_FACTOR: f32 = 0.01;
const UNLOCK_SCRIPT: &str = r#"
if redis.call("GET", KEYS[1]) == ARGV[1] then
  return redis.call("DEL", KEYS[1])
else
  return 0
end
"#;
const EXTEND_SCRIPT: &str = r#"
if redis.call("get", KEYS[1]) ~= ARGV[1] then
  return 0
else
  if redis.call("set", KEYS[1], ARGV[1], "PX", ARGV[2]) ~= nil then
    return 1
  else
    return 0
  end
end
"#;

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Redis error: {0}")]
    Redis(#[from] redis::RedisError),

    #[error("Resource is unavailable")]
    Unavailable,

    #[error("TTL exceeded")]
    TtlExceeded,

    #[error("TTL too large")]
    TtlTooLarge,

    #[error("Redis connection failed for all servers")]
    RedisConnectionFailed,

    #[error("Redis key mismatch: expected value does not match actual value")]
    RedisKeyMismatch,

    #[error("Redis key not found")]
    RedisKeyNotFound,
}

/// The lock manager.
///
/// Implements the necessary functionality to acquire and release locks
/// and handles the Redis connections.
#[derive(Debug, Clone)]
pub struct LockManager {
    lock_manager_inner: Arc<LockManagerInner>,
    retry_count: u32,
    retry_delay: Duration,
}

#[derive(Debug, Clone)]
struct LockManagerInner {
    /// List of all Redis clients
    pub servers: Vec<Client>,
    quorum: u32,
}

/// A distributed lock that can be acquired and released across multiple Redis instances.
///
/// A `Lock` represents a distributed lock in Redis.
/// The lock is associated with a resource, identified by a unique key, and a value that identifies
/// the lock owner. The `LockManager` is responsible for managing the acquisition, release, and extension
/// of locks.
#[derive(Debug)]
pub struct Lock {
    /// The resource to lock. Will be used as the key in Redis.
    pub resource: Vec<u8>,
    /// The value for this lock.
    pub val: Vec<u8>,
    /// Time the lock is still valid.
    /// Should only be slightly smaller than the requested TTL.
    pub validity_time: usize,
    /// Used to limit the lifetime of a lock to its lock manager.
    pub lock_manager: LockManager,
}

/// Upon dropping the guard, `LockManager::unlock` will be ran synchronously on the executor.
///
/// This is known to block the tokio runtime if this happens inside of the context of a tokio runtime
/// if `tokio-comp` is enabled as a feature on this crate or the `redis` crate.
///
/// To eliminate this risk, if the `tokio-comp` flag is enabled, the `Drop` impl will not be compiled,
/// meaning that dropping the `LockGuard` will be a no-op.
/// Under this circumstance, `LockManager::unlock` can be called manually using the inner `lock` at the appropriate
/// point to release the lock taken in `Redis`.
#[derive(Debug)]
pub struct LockGuard {
    pub lock: Lock,
}

/// Dropping this guard inside the context of a tokio runtime if `tokio-comp` is enabled
/// will block the tokio runtime.
/// Because of this, the guard is not compiled if `tokio-comp` is enabled.
#[cfg(not(feature = "tokio-comp"))]
impl Drop for LockGuard {
    fn drop(&mut self) {
        futures::executor::block_on(self.lock.lock_manager.unlock(&self.lock));
    }
}

impl LockManager {
    /// Create a new lock manager instance, defined by the given Redis connection uris.
    /// Quorum is defined to be N/2+1, with N being the number of given Redis instances.
    ///
    /// Sample URI: `"redis://127.0.0.1:6379"`
    pub fn new<T: IntoConnectionInfo>(uris: Vec<T>) -> LockManager {
        let quorum = (uris.len() as u32) / 2 + 1;

        let servers: Vec<Client> = uris
            .into_iter()
            .map(|uri| Client::open(uri).unwrap())
            .collect();

        LockManager {
            lock_manager_inner: Arc::new(LockManagerInner { servers, quorum }),
            retry_count: DEFAULT_RETRY_COUNT,
            retry_delay: DEFAULT_RETRY_DELAY,
        }
    }

    /// Get 20 random bytes from the pseudorandom interface.
    pub fn get_unique_lock_id(&self) -> io::Result<Vec<u8>> {
        let mut buf = [0u8; 20];
        thread_rng().fill_bytes(&mut buf);
        Ok(buf.to_vec())
    }

    /// Set retry count and retry delay.
    ///
    /// Retry count defaults to `3`.
    /// Retry delay defaults to `200`.
    pub fn set_retry(&mut self, count: u32, delay: Duration) {
        self.retry_count = count;
        self.retry_delay = delay;
    }

    async fn lock_instance(
        client: &redis::Client,
        resource: &[u8],
        val: Vec<u8>,
        ttl: usize,
    ) -> bool {
        let mut con = match client.get_multiplexed_async_connection().await {
            Err(_) => return false,
            Ok(val) => val,
        };
        let result: RedisResult<Value> = redis::cmd("SET")
            .arg(resource)
            .arg(val)
            .arg("NX")
            .arg("PX")
            .arg(ttl)
            .query_async(&mut con)
            .await;

        match result {
            Ok(Okay) => true,
            Ok(_) | Err(_) => false,
        }
    }

    async fn extend_lock_instance(
        client: &redis::Client,
        resource: &[u8],
        val: &[u8],
        ttl: usize,
    ) -> bool {
        let mut con = match client.get_async_connection().await {
            Err(_) => return false,
            Ok(val) => val,
        };
        let script = redis::Script::new(EXTEND_SCRIPT);
        let result: RedisResult<i32> = script
            .key(resource)
            .arg(val)
            .arg(ttl)
            .invoke_async(&mut con)
            .await;
        match result {
            Ok(val) => val == 1,
            Err(_) => false,
        }
    }

    async fn unlock_instance(client: &redis::Client, resource: &[u8], val: &[u8]) -> bool {
        let mut con = match client.get_async_connection().await {
            Err(_) => return false,
            Ok(val) => val,
        };
        let script = redis::Script::new(UNLOCK_SCRIPT);
        let result: RedisResult<i32> = script.key(resource).arg(val).invoke_async(&mut con).await;
        match result {
            Ok(val) => val == 1,
            Err(_) => false,
        }
    }

    // Can be used for creating or extending a lock
    async fn exec_or_retry<'a, T, Fut>(
        &'a self,
        resource: &[u8],
        value: &[u8],
        ttl: usize,
        lock: T,
    ) -> Result<Lock, LockError>
    where
        T: Fn(&'a Client) -> Fut,
        Fut: Future<Output = bool>,
    {
        for _ in 0..self.retry_count {
            let start_time = Instant::now();
            let n = join_all(self.lock_manager_inner.servers.iter().map(&lock))
                .await
                .into_iter()
                .fold(0, |count, locked| if locked { count + 1 } else { count });

            let drift = (ttl as f32 * CLOCK_DRIFT_FACTOR) as usize + 2;
            let elapsed = start_time.elapsed();
            let elapsed_ms =
                elapsed.as_secs() as usize * 1000 + elapsed.subsec_nanos() as usize / 1_000_000;
            if ttl <= drift + elapsed_ms {
                return Err(LockError::TtlExceeded);
            }
            let validity_time = ttl
                - drift
                - elapsed.as_secs() as usize * 1000
                - elapsed.subsec_nanos() as usize / 1_000_000;

            if n >= self.lock_manager_inner.quorum && validity_time > 0 {
                return Ok(Lock {
                    lock_manager: self.clone(),
                    resource: resource.to_vec(),
                    val: value.to_vec(),
                    validity_time,
                });
            } else {
                join_all(
                    self.lock_manager_inner
                        .servers
                        .iter()
                        .map(|client| Self::unlock_instance(client, resource, value)),
                )
                .await;
            }

            let retry_delay: u64 = self
                .retry_delay
                .as_millis()
                .try_into()
                .map_err(|_| LockError::TtlTooLarge)?;
            let n = thread_rng().gen_range(0..retry_delay);
            tokio::time::sleep(Duration::from_millis(n)).await
        }

        Err(LockError::Unavailable)
    }

    // Query Redis for a key's value and keep trying each server until a successful result is returned
    async fn query_redis_for_key_value(
        &self,
        resource: &[u8],
    ) -> Result<Option<Vec<u8>>, LockError> {
        for client in &self.lock_manager_inner.servers {
            let mut con = match client.get_async_connection().await {
                Ok(con) => con,
                Err(_) => continue, // If connection fails, try the next server
            };

            let result: RedisResult<Option<Vec<u8>>> =
                redis::cmd("GET").arg(resource).query_async(&mut con).await;

            match result {
                Ok(val) => return Ok(val),
                Err(_) => continue, // If query fails, try the next server
            }
        }

        Err(LockError::RedisConnectionFailed) // All servers failed
    }

    /// Unlock the given lock.
    ///
    /// Unlock is best effort. It will simply try to contact all instances
    /// and remove the key.
    pub async fn unlock(&self, lock: &Lock) {
        join_all(
            self.lock_manager_inner
                .servers
                .iter()
                .map(|client| Self::unlock_instance(client, &lock.resource, &lock.val)),
        )
        .await;
    }

    /// Acquire the lock for the given resource and the requested TTL.
    ///
    /// If it succeeds, a `Lock` instance is returned,
    /// including the value and the validity time
    ///
    /// If it fails. `None` is returned.
    /// A user should retry after a short wait time.
    ///
    /// May return `LockError::TtlTooLarge` if `ttl` is too large.
    pub async fn lock(&self, resource: &[u8], ttl: Duration) -> Result<Lock, LockError> {
        let val = self.get_unique_lock_id().map_err(LockError::Io)?;
        let ttl = ttl
            .as_millis()
            .try_into()
            .map_err(|_| LockError::TtlTooLarge)?;

        self.exec_or_retry(resource, &val.clone(), ttl, move |client| {
            Self::lock_instance(client, resource, val.clone(), ttl)
        })
        .await
    }

    /// Loops until the lock is acquired.
    ///
    /// The lock is placed in a guard that will unlock the lock when the guard is dropped.
    ///
    /// May return `LockError::TtlTooLarge` if `ttl` is too large.
    #[cfg(feature = "async-std-comp")]
    pub async fn acquire(&self, resource: &[u8], ttl: Duration) -> Result<LockGuard, LockError> {
        let lock = self.acquire_no_guard(resource, ttl).await?;
        Ok(LockGuard { lock })
    }

    /// Loops until the lock is acquired.
    ///
    /// Either lock's value must expire after the ttl has elapsed,
    /// or `LockManager::unlock` must be called to allow other clients to lock the same resource.
    ///
    /// May return `LockError::TtlTooLarge` if `ttl` is too large.
    pub async fn acquire_no_guard(
        &self,
        resource: &[u8],
        ttl: Duration,
    ) -> Result<Lock, LockError> {
        loop {
            match self.lock(resource, ttl).await {
                Ok(lock) => return Ok(lock),
                Err(LockError::TtlTooLarge) => return Err(LockError::TtlTooLarge),
                Err(_) => continue,
            }
        }
    }

    /// Extend the given lock by given time in milliseconds
    pub async fn extend(&self, lock: &Lock, ttl: Duration) -> Result<Lock, LockError> {
        let ttl = ttl
            .as_millis()
            .try_into()
            .map_err(|_| LockError::TtlTooLarge)?;

        self.exec_or_retry(&lock.resource, &lock.val, ttl, move |client| {
            Self::extend_lock_instance(client, &lock.resource, &lock.val, ttl)
        })
        .await
    }

    /// Checks if the given lock has been freed (i.e., is no longer held).
    ///
    /// This method queries Redis to determine if the key associated with the lock
    /// is still present and matches the value of this lock. If the key is missing
    /// or the value does not match, the lock is considered freed.
    ///
    /// # Returns
    ///
    /// `Ok(true)` if the lock is considered freed (either because the key does not exist
    /// or the value does not match), otherwise `Ok(false)`. Returns an error if a Redis
    /// connection or query fails.
    pub async fn is_freed(&self, lock: &Lock) -> Result<bool, LockError> {
        match self.query_redis_for_key_value(&lock.resource).await? {
            Some(val) => {
                if val != lock.val {
                    Err(LockError::RedisKeyMismatch)
                } else {
                    Ok(false) // Key is present and matches the lock value
                }
            }
            None => Err(LockError::RedisKeyNotFound), // Key does not exist
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use once_cell::sync::Lazy;
    use testcontainers::clients::Cli;
    use testcontainers::images::redis::Redis;
    use testcontainers::{Container, RunnableImage};

    use super::*;

    type Containers = Vec<Container<'static, Redis>>;

    static DOCKER: Lazy<Cli> = Lazy::new(Cli::docker);

    fn is_normal<T: Sized + Send + Sync + Unpin>() {}

    fn create_clients() -> (Containers, Vec<String>) {
        let containers: Containers = (1..=3)
            .map(|_| {
                let image = RunnableImage::from(Redis::default()).with_tag("7-alpine");
                DOCKER.run(image)
            })
            .collect();

        let addresses = containers
            .iter()
            .map(|node| format!("redis://localhost:{}", node.get_host_port_ipv4(6379)))
            .collect();

        (containers, addresses)
    }

    // Test that the LockManager is Send + Sync
    #[test]
    fn test_is_normal() {
        is_normal::<LockManager>();
        is_normal::<LockError>();
        is_normal::<Lock>();
        is_normal::<LockGuard>();
    }

    #[tokio::test]
    async fn test_lock_get_unique_id() -> Result<()> {
        let rl = LockManager::new(Vec::<String>::new());
        assert_eq!(rl.get_unique_lock_id()?.len(), 20);

        Ok(())
    }

    #[tokio::test]
    async fn test_lock_get_unique_id_uniqueness() -> Result<()> {
        let rl = LockManager::new(Vec::<String>::new());

        let id1 = rl.get_unique_lock_id()?;
        let id2 = rl.get_unique_lock_id()?;

        assert_eq!(20, id1.len());
        assert_eq!(20, id2.len());
        assert_ne!(id1, id2);

        Ok(())
    }

    #[tokio::test]
    async fn test_lock_valid_instance() {
        let (_containers, addresses) = create_clients();

        let rl = LockManager::new(addresses.clone());

        assert_eq!(3, rl.lock_manager_inner.servers.len());
        assert_eq!(2, rl.lock_manager_inner.quorum);
    }

    #[tokio::test]
    async fn test_lock_direct_unlock_fails() -> Result<()> {
        let (_containers, addresses) = create_clients();

        let rl = LockManager::new(addresses.clone());
        let key = rl.get_unique_lock_id()?;

        let val = rl.get_unique_lock_id()?;
        assert!(!LockManager::unlock_instance(&rl.lock_manager_inner.servers[0], &key, &val).await);

        Ok(())
    }

    #[tokio::test]
    async fn test_lock_direct_unlock_succeeds() -> Result<()> {
        let (_containers, addresses) = create_clients();

        let rl = LockManager::new(addresses.clone());
        let key = rl.get_unique_lock_id()?;

        let val = rl.get_unique_lock_id()?;
        let mut con = rl.lock_manager_inner.servers[0].get_multiplexed_async_connection()?;
        redis::cmd("SET")
            .arg(&*key)
            .arg(&*val)
            .exec_async(&mut con)
            .await?;

        assert!(LockManager::unlock_instance(&rl.lock_manager_inner.servers[0], &key, &val).await);

        Ok(())
    }

    #[tokio::test]
    async fn test_lock_direct_lock_succeeds() -> Result<()> {
        let (_containers, addresses) = create_clients();

        let rl = LockManager::new(addresses.clone());
        let key = rl.get_unique_lock_id()?;

        let val = rl.get_unique_lock_id()?;
        let mut con = rl.lock_manager_inner.servers[0].get_multiplexed_async_connection()?;

        redis::cmd("DEL").arg(&*key).exec_async(&mut con).await?;
        assert!(
            LockManager::lock_instance(&rl.lock_manager_inner.servers[0], &key, val.clone(), 1000)
                .await
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_lock_unlock() -> Result<()> {
        let (_containers, addresses) = create_clients();

        let rl = LockManager::new(addresses.clone());
        let key = rl.get_unique_lock_id()?;

        let val = rl.get_unique_lock_id()?;
        let mut con = rl.lock_manager_inner.servers[0].get_connection()?;
        let _: () = redis::cmd("SET")
            .arg(&*key)
            .arg(&*val)
            .query(&mut con)
            .unwrap();

        let lock = Lock {
            lock_manager: rl.clone(),
            resource: key,
            val,
            validity_time: 0,
        };

        rl.unlock(&lock).await;

        Ok(())
    }

    #[tokio::test]
    async fn test_lock_lock() -> Result<()> {
        let (_containers, addresses) = create_clients();

        let rl = LockManager::new(addresses.clone());

        let key = rl.get_unique_lock_id()?;
        match rl.lock(&key, Duration::from_millis(1000)).await {
            Ok(lock) => {
                assert_eq!(key, lock.resource);
                assert_eq!(20, lock.val.len());
                assert!(
                    lock.validity_time > 0,
                    "validity time: {}",
                    lock.validity_time
                );
            }
            Err(e) => panic!("{:?}", e),
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_lock_lock_unlock() -> Result<()> {
        let (_containers, addresses) = create_clients();

        let rl = LockManager::new(addresses.clone());
        let rl2 = LockManager::new(addresses.clone());

        let key = rl.get_unique_lock_id()?;

        let lock = rl.lock(&key, Duration::from_millis(1000)).await.unwrap();
        assert!(
            lock.validity_time > 0,
            "validity time: {}",
            lock.validity_time
        );

        if let Ok(_l) = rl2.lock(&key, Duration::from_millis(1000)).await {
            panic!("Lock acquired, even though it should be locked")
        }

        rl.unlock(&lock).await;

        match rl2.lock(&key, Duration::from_millis(1000)).await {
            Ok(l) => assert!(l.validity_time > 0),
            Err(_) => panic!("Lock couldn't be acquired"),
        }

        Ok(())
    }

    #[cfg(all(not(feature = "tokio-comp"), feature = "async-std-comp"))]
    #[tokio::test]
    async fn test_lock_lock_unlock_raii() -> Result<()> {
        let (_containers, addresses) = create_clients();

        let rl = LockManager::new(addresses.clone());
        let rl2 = LockManager::new(addresses.clone());
        let key = rl.get_unique_lock_id()?;

        async {
            let lock_guard = rl.acquire(&key, Duration::from_millis(1000)).await.unwrap();
            let lock = &lock_guard.lock;
            assert!(
                lock.validity_time > 0,
                "validity time: {}",
                lock.validity_time
            );

            if let Ok(_l) = rl2.lock(&key, Duration::from_millis(1000)).await {
                panic!("Lock acquired, even though it should be locked")
            }
        }
        .await;

        match rl2.lock(&key, Duration::from_millis(1000)).await {
            Ok(l) => assert!(l.validity_time > 0),
            Err(_) => panic!("Lock couldn't be acquired"),
        }

        Ok(())
    }

    #[cfg(feature = "tokio-comp")]
    #[tokio::test]
    async fn test_lock_raii_does_not_unlock_with_tokio_enabled() -> Result<()> {
        let (_containers, addresses) = create_clients();

        let rl1 = LockManager::new(addresses.clone());
        let rl2 = LockManager::new(addresses.clone());
        let key = rl1.get_unique_lock_id()?;

        async {
            let lock_guard = rl1
                .acquire(&key, Duration::from_millis(10_000))
                .await
                .expect("LockManage rl1 should be able to acquire lock");
            let lock = &lock_guard.lock;
            assert!(
                lock.validity_time > 0,
                "validity time: {}",
                lock.validity_time
            );

            // Acquire lock2 and assert it can't be acquired
            if let Ok(_l) = rl2.lock(&key, Duration::from_millis(1000)).await {
                panic!("Lock acquired, even though it should be locked")
            }
        }
        .await;

        if let Ok(_) = rl2.lock(&key, Duration::from_millis(1000)).await {
            panic!("Lock couldn't be acquired");
        }

        Ok(())
    }

    #[cfg(feature = "async-std-comp")]
    #[tokio::test]
    async fn test_lock_extend_lock() -> Result<()> {
        let (_containers, addresses) = create_clients();

        let rl1 = LockManager::new(addresses.clone());
        let rl2 = LockManager::new(addresses.clone());

        let key = rl1.get_unique_lock_id()?;

        async {
            let lock1 = rl1
                .acquire(&key, Duration::from_millis(1000))
                .await
                .unwrap();

            // Wait half a second before locking again
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            rl1.extend(&lock1.lock, Duration::from_millis(1000))
                .await
                .unwrap();

            // Wait another half a second to see if lock2 can unlock
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            // Assert lock2 can't access after extended lock
            match rl2.lock(&key, Duration::from_millis(1000)).await {
                Ok(_) => panic!("Expected an error when extending the lock but didn't receive one"),
                Err(e) => match e {
                    LockError::Unavailable => (),
                    _ => panic!("Unexpected error when extending lock"),
                },
            }
        }
        .await;

        Ok(())
    }

    #[cfg(feature = "async-std-comp")]
    #[tokio::test]
    async fn test_lock_extend_lock_releases() -> Result<()> {
        let (_containers, addresses) = create_clients();

        let rl1 = LockManager::new(addresses.clone());
        let rl2 = LockManager::new(addresses.clone());

        let key = rl1.get_unique_lock_id()?;

        async {
            // Create 500ms lock and immediately extend 500ms
            let lock1 = rl1.acquire(&key, Duration::from_millis(500)).await.unwrap();
            rl1.extend(&lock1.lock, Duration::from_millis(500))
                .await
                .unwrap();

            // Wait one second for the lock to expire
            tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;

            // Assert rl2 can lock with the key now
            match rl2.lock(&key, Duration::from_millis(1000)).await {
                Err(_) => {
                    panic!("Unexpected error when trying to claim free lock after extend expired")
                }
                _ => (),
            }

            // Also assert rl1 can't reuse lock1
            match rl1.extend(&lock1.lock, Duration::from_millis(1000)).await {
                Ok(_) => panic!("Did not expect OK() when re-extending rl1"),
                Err(e) => match e {
                    LockError::Unavailable => (),
                    _ => panic!("Expected lockError::Unavailable when re-extending rl1"),
                },
            }
        }
        .await;

        Ok(())
    }

    #[tokio::test]
    async fn test_lock_with_short_ttl_and_retries() -> Result<()> {
        let (_containers, addresses) = create_clients();

        let mut rl = LockManager::new(addresses.clone());
        // Set a high retry count to ensure retries happen
        rl.set_retry(10, Duration::from_millis(10)); // Retry 10 times with 10 milliseconds delay

        let key = rl.get_unique_lock_id()?;

        // Use a very short TTL
        let ttl = Duration::from_millis(1);

        // Acquire lock
        let lock_result = rl.lock(&key, ttl).await;

        // Check if the error returned is TtlExceeded
        match lock_result {
            Err(LockError::TtlExceeded) => (), // Test passes
            _ => panic!("Expected LockError::TtlExceeded, but got {:?}", lock_result),
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_lock_ttl_duration_conversion_error() {
        let (_containers, addresses) = create_clients();
        let rl = LockManager::new(addresses.clone());
        let key = rl.get_unique_lock_id().unwrap();

        // Too big Duration, fails - technical limit is from_millis(u64::MAX)
        let ttl = Duration::from_secs(u64::MAX);
        match rl.lock(&key, ttl).await {
            Ok(_) => panic!("Expected LockError::TtlTooLarge"),
            Err(_) => (), // Test passes
        }
    }

    #[tokio::test]
    async fn test_lock_send_lock_manager() {
        let (_containers, addresses) = create_clients();
        let rl = LockManager::new(addresses.clone());

        let lock = rl
            .lock(b"resource", std::time::Duration::from_millis(1000))
            .await
            .unwrap();

        // Send the lock and entry through the channel
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        tx.send(("some info", lock, rl)).await.unwrap();

        let j = tokio::spawn(async move {
            // Retrieve from channel and use
            if let Some((_entry, lock, rl)) = rx.recv().await {
                rl.unlock(&lock).await;
            }
        });
        let _ = j.await;
    }

    #[tokio::test]
    async fn test_lock_state_in_multiple_threads() {
        let (_containers, addresses) = create_clients();
        let rl = LockManager::new(addresses.clone());

        let lock1 = rl
            .lock(b"resource_1", std::time::Duration::from_millis(1000))
            .await
            .unwrap();

        let lock1 = Arc::new(lock1);
        // Send the lock and entry through the channel
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        tx.send(("some info", lock1.clone(), rl.clone()))
            .await
            .unwrap();

        let j = tokio::spawn(async move {
            // Retrieve from channel and use
            if let Some((_entry, lock1, rl)) = rx.recv().await {
                rl.unlock(&lock1).await;
            }
        });
        let _ = j.await;

        match rl.is_freed(&lock1).await {
            Ok(freed) => assert!(freed, "Lock should be freed after unlock"),
            Err(LockError::RedisKeyNotFound) => {
                assert!(true, "RedisKeyNotFound is expected if key is missing")
            }
            Err(e) => panic!("Unexpected error: {:?}", e),
        };

        let lock2 = rl
            .lock(b"resource_2", std::time::Duration::from_millis(1000))
            .await
            .unwrap();
        rl.unlock(&lock2).await;

        match rl.is_freed(&lock2).await {
            Ok(freed) => assert!(freed, "Lock should be freed after unlock"),
            Err(LockError::RedisKeyNotFound) => {
                assert!(true, "RedisKeyNotFound is expected if key is missing")
            }
            Err(e) => panic!("Unexpected error: {:?}", e),
        };
    }

    #[tokio::test]
    async fn test_redis_value_matches_lock_value() {
        let (_containers, addresses) = create_clients();
        let rl = LockManager::new(addresses.clone());

        let lock = rl
            .lock(b"resource_1", std::time::Duration::from_millis(1000))
            .await
            .unwrap();

        // Ensure Redis key is correctly set and matches the lock value
        let mut con = rl.lock_manager_inner.servers[0]
            .get_async_connection()
            .await
            .unwrap();
        let redis_val: Option<Vec<u8>> = redis::cmd("GET")
            .arg(&lock.resource)
            .query_async(&mut con)
            .await
            .unwrap();

        eprintln!(
            "Debug: Expected value in Redis: {:?}, Actual value in Redis: {:?}",
            Some(lock.val.as_slice()),
            redis_val.as_deref()
        );

        assert_eq!(
            redis_val.as_deref(),
            Some(lock.val.as_slice()),
            "Redis value should match lock value"
        );
    }

    #[tokio::test]
    async fn test_is_not_freed_after_lock() {
        let (_containers, addresses) = create_clients();
        let rl = LockManager::new(addresses.clone());

        let lock = rl
            .lock(b"resource_1", std::time::Duration::from_millis(1000))
            .await
            .unwrap();

        match rl.is_freed(&lock).await {
            Ok(freed) => assert!(!freed, "Lock should not be freed after it is acquired"),
            Err(LockError::RedisKeyMismatch) => {
                panic!("Redis key mismatch should not occur for a valid lock")
            }
            Err(LockError::RedisKeyNotFound) => {
                panic!("Redis key not found should not occur for a valid lock")
            }
            Err(e) => panic!("Unexpected error: {:?}", e),
        };
    }

    #[tokio::test]
    async fn test_is_freed_after_manual_unlock() {
        let (_containers, addresses) = create_clients();
        let rl = LockManager::new(addresses.clone());

        let lock = rl
            .lock(b"resource_2", std::time::Duration::from_millis(1000))
            .await
            .unwrap();

        rl.unlock(&lock).await;

        match rl.is_freed(&lock).await {
            Ok(freed) => assert!(freed, "Lock should be freed after unlock"),
            Err(LockError::RedisKeyNotFound) => {
                assert!(true, "RedisKeyNotFound is expected if key is missing")
            }
            Err(e) => panic!("Unexpected error: {:?}", e),
        };
    }

    #[tokio::test]
    async fn test_is_freed_when_key_missing_in_redis() {
        let (_containers, addresses) = create_clients();
        let rl = LockManager::new(addresses.clone());

        let lock = rl
            .lock(b"resource_3", std::time::Duration::from_millis(1000))
            .await
            .unwrap();

        // Manually delete the key in Redis to simulate it being missing
        let mut con = rl.lock_manager_inner.servers[0]
            .get_async_connection()
            .await
            .unwrap();
        redis::cmd("DEL")
            .arg(&lock.resource)
            .query_async::<()>(&mut con)
            .await
            .unwrap();

        match rl.is_freed(&lock).await {
            Ok(freed) => assert!(
                freed,
                "Lock should be marked as freed when key is missing in Redis"
            ),
            Err(LockError::RedisKeyNotFound) => assert!(
                true,
                "RedisKeyNotFound is expected when key is missing in Redis"
            ),
            Err(e) => panic!("Unexpected error: {:?}", e),
        };
    }

    #[tokio::test]
    async fn test_is_freed_handles_redis_connection_failure() {
        let (_containers, _) = create_clients();
        let rl = LockManager::new(Vec::<String>::new()); // No Redis clients, simulate failure

        let lock_result = rl
            .lock(b"resource_4", std::time::Duration::from_millis(1000))
            .await;

        match lock_result {
            Ok(lock) => {
                // Since there are no clients, any check with Redis will fail
                match rl.is_freed(&lock).await {
                    Ok(freed) => panic!("Expected failure due to Redis connection, but got Ok with freed status: {}", freed),
                    Err(LockError::RedisConnectionFailed) => assert!(true, "Expected RedisConnectionFailed when all Redis connections fail"),
                    Err(e) => panic!("Unexpected error: {:?}", e),
                }
            }
            Err(LockError::Unavailable) => {
                // Expected error, the test should pass in this scenario
                assert!(true);
            }
            Err(e) => panic!("Unexpected error while acquiring lock: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_redis_connection_failed() {
        let (_containers, _) = create_clients();
        let rl = LockManager::new(Vec::<String>::new()); // No Redis clients, simulate failure

        let lock_result = rl
            .lock(b"resource_5", std::time::Duration::from_millis(1000))
            .await;

        match lock_result {
            Ok(lock) => match rl.is_freed(&lock).await {
                Err(LockError::RedisConnectionFailed) => assert!(
                    true,
                    "Expected RedisConnectionFailed when all Redis connections fail"
                ),
                Ok(_) => panic!("Expected RedisConnectionFailed, but got Ok"),
                Err(e) => panic!("Unexpected error: {:?}", e),
            },
            Err(LockError::Unavailable) => {
                // Expected error, the test should pass in this scenario
                assert!(true);
            }
            Err(e) => panic!("Unexpected error while acquiring lock: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_redis_key_mismatch() {
        let (_containers, addresses) = create_clients();
        let rl = LockManager::new(addresses.clone());

        let lock = rl
            .lock(b"resource_6", std::time::Duration::from_millis(1000))
            .await
            .unwrap();

        // Set a different value for the same key to simulate a mismatch
        let mut con = rl.lock_manager_inner.servers[0]
            .get_async_connection()
            .await
            .unwrap();
        let different_value: Vec<u8> = vec![1, 2, 3, 4, 5]; // Different value
        redis::cmd("SET")
            .arg(&lock.resource)
            .arg(different_value)
            .query_async::<()>(&mut con)
            .await
            .unwrap();

        // Now check if is_freed identifies the mismatch correctly
        match rl.is_freed(&lock).await {
            Err(LockError::RedisKeyMismatch) => assert!(
                true,
                "Expected RedisKeyMismatch when key value does not match the lock value"
            ),
            Ok(_) => panic!("Expected RedisKeyMismatch, but got Ok"),
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_redis_key_not_found() {
        let (_containers, addresses) = create_clients();
        let rl = LockManager::new(addresses.clone());

        let lock = rl
            .lock(b"resource_7", std::time::Duration::from_millis(1000))
            .await
            .unwrap();

        // Manually delete the key in Redis to simulate it being missing
        let mut con = rl.lock_manager_inner.servers[0]
            .get_async_connection()
            .await
            .unwrap();
        redis::cmd("DEL")
            .arg(&lock.resource)
            .query_async::<()>(&mut con)
            .await
            .unwrap();

        match rl.is_freed(&lock).await {
            Err(LockError::RedisKeyNotFound) => assert!(
                true,
                "Expected RedisKeyNotFound when key is missing in Redis"
            ),
            Ok(_) => panic!("Expected RedisKeyNotFound, but got Ok"),
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }
}
