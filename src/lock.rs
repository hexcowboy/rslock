use std::fmt;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::join_all;
use rand::{rng, Rng, RngExt};
use redis::aio::{ConnectionLike, MultiplexedConnection};
#[cfg(feature = "cluster")]
use redis::cluster::ClusterClient;
#[cfg(feature = "cluster")]
use redis::cluster_async::ClusterConnection;
use redis::Value::Okay;
use redis::{
    Client, Cmd, IntoConnectionInfo, Pipeline, RedisError, RedisFuture, RedisResult, Value,
};

use crate::resource::{LockResource, ToLockResource};

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

    #[error("Redis connection failed.")]
    RedisFailedToEstablishConnection,

    #[error("Redis key mismatch: expected value does not match actual value")]
    RedisKeyMismatch,

    #[error("Redis key not found")]
    RedisKeyNotFound,
    #[error("A mutex was poisoned")]
    MutexPoisoned,
}

//This is in place to make it easier to swap to just an std-async io implementaiton ??
type Mutex<T> = tokio::sync::Mutex<T>;
type MutexGuard<'a, K> = tokio::sync::MutexGuard<'a, K>;

/// The lock manager.
///
/// Implements the necessary functionality to acquire and release locks
/// and handles the Redis connections.
#[derive(Debug, Clone)]
pub struct LockManager {
    lock_manager_inner: Arc<Mutex<LockManagerInner>>,
    retry_count: u32,
    retry_delay: Duration,
}

#[derive(Debug, Clone)]
struct LockManagerInner {
    /// List of all Redis clients
    pub servers: Vec<RestorableConnection>,
}

impl LockManagerInner {
    fn get_quorum(&self) -> u32 {
        (self.servers.len() as u32) / 2 + 1
    }
}

#[derive(Clone)]
struct RestorableConnection {
    client: RedisClient,
    con: Arc<Mutex<Option<RedisConnection>>>,
}

impl RestorableConnection {
    pub fn new(client: Client) -> Self {
        Self {
            client: RedisClient::Standalone(client),
            con: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    #[cfg(feature = "cluster")]
    pub fn new_cluster(client: ClusterClient) -> Self {
        Self {
            client: RedisClient::Cluster(client),
            con: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    async fn connect(&self) -> RedisResult<RedisConnection> {
        match &self.client {
            RedisClient::Standalone(client) => client
                .get_multiplexed_async_connection()
                .await
                .map(RedisConnection::Standalone),
            #[cfg(feature = "cluster")]
            RedisClient::Cluster(client) => client
                .get_async_connection()
                .await
                .map(RedisConnection::Cluster),
        }
    }

    pub async fn get_connection(&mut self) -> Result<RedisConnection, LockError> {
        let mut lock = self.con.lock().await;
        if lock.is_none() {
            *lock = Some(self.connect().await.map_err(LockError::Redis)?);
        }
        match (*lock).clone() {
            Some(conn) => Ok(conn),
            None => Err(LockError::RedisFailedToEstablishConnection),
        }
    }

    pub async fn recover(&mut self, error: RedisError) -> Result<(), LockError> {
        //We need to rebuild the connection
        if !error.is_unrecoverable_error() {
            Ok(())
        } else {
            let mut lock = self.con.lock().await;
            *lock = Some(self.connect().await.map_err(LockError::Redis)?);
            Ok(())
        }
    }
}

#[derive(Clone)]
enum RedisClient {
    Standalone(Client),
    #[cfg(feature = "cluster")]
    Cluster(ClusterClient),
}

impl fmt::Debug for RedisClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Standalone(_) => formatter.write_str("Standalone"),
            #[cfg(feature = "cluster")]
            Self::Cluster(_) => formatter.write_str("Cluster"),
        }
    }
}

#[derive(Clone)]
enum RedisConnection {
    Standalone(MultiplexedConnection),
    #[cfg(feature = "cluster")]
    Cluster(ClusterConnection),
}

impl fmt::Debug for RedisConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Standalone(_) => formatter.write_str("Standalone"),
            #[cfg(feature = "cluster")]
            Self::Cluster(_) => formatter.write_str("Cluster"),
        }
    }
}

impl ConnectionLike for RedisConnection {
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        match self {
            Self::Standalone(connection) => connection.req_packed_command(cmd),
            #[cfg(feature = "cluster")]
            Self::Cluster(connection) => connection.req_packed_command(cmd),
        }
    }

    fn req_packed_commands<'a>(
        &'a mut self,
        pipeline: &'a Pipeline,
        offset: usize,
        count: usize,
    ) -> RedisFuture<'a, Vec<Value>> {
        match self {
            Self::Standalone(connection) => connection.req_packed_commands(pipeline, offset, count),
            #[cfg(feature = "cluster")]
            Self::Cluster(connection) => connection.req_packed_commands(pipeline, offset, count),
        }
    }

    fn get_db(&self) -> i64 {
        match self {
            Self::Standalone(connection) => connection.get_db(),
            #[cfg(feature = "cluster")]
            Self::Cluster(connection) => connection.get_db(),
        }
    }
}

impl fmt::Debug for RestorableConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RestorableConnection")
            .field("client", &self.client)
            .field("con", &self.con)
            .finish()
    }
}

impl RestorableConnection {
    async fn lock(&mut self, resource: &LockResource<'_>, val: &[u8], ttl: usize) -> bool {
        let mut con = match self.get_connection().await {
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
            Ok(_) => false,
            Err(e) => {
                //We don't have to do anything special, it's up to the caller to retry or back out
                let _ = self.recover(e).await;
                false
            }
        }
    }

    async fn extend(&mut self, resource: &LockResource<'_>, val: &[u8], ttl: usize) -> bool {
        let mut con = match self.get_connection().await {
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
            Err(e) => {
                //We don't have to do anything special, it's up to the caller to retry or back out
                let _ = self.recover(e).await;
                false
            }
        }
    }

    async fn unlock(&mut self, resource: impl ToLockResource<'_>, val: &[u8]) -> bool {
        let resource = resource.to_lock_resource();
        let mut con = match self.get_connection().await {
            Err(_) => return false,
            Ok(val) => val,
        };
        let script = redis::Script::new(UNLOCK_SCRIPT);
        let result: RedisResult<i32> = script.key(resource).arg(val).invoke_async(&mut con).await;
        match result {
            Ok(val) => val == 1,
            Err(e) => {
                //We don't have to do anything special, it's up to the caller to retry or back out
                let _ = self.recover(e).await;
                false
            }
        }
    }

    async fn query(&mut self, resource: &[u8]) -> RedisResult<Option<Vec<u8>>> {
        let mut con = match self.get_connection().await {
            Ok(con) => con,
            Err(_e) => return Ok(None),
        };
        let result: RedisResult<Option<Vec<u8>>> =
            redis::cmd("GET").arg(resource).query_async(&mut con).await;
        result
    }
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

enum Operation {
    Lock,
    Extend,
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
    ///
    /// Sample URI: `"redis://127.0.0.1:6379"`
    pub fn new<T: IntoConnectionInfo>(uris: Vec<T>) -> LockManager {
        let servers: Vec<Client> = uris
            .into_iter()
            .map(|uri| Client::open(uri).unwrap())
            .collect();

        Self::from_clients(servers)
    }

    /// Create a new lock manager instance, defined by the given Redis clients.
    /// Quorum is defined to be N/2+1, with N being the number of given Redis instances.
    pub fn from_clients(clients: Vec<Client>) -> LockManager {
        let clients: Vec<RestorableConnection> =
            clients.into_iter().map(RestorableConnection::new).collect();
        LockManager {
            lock_manager_inner: Arc::new(Mutex::new(LockManagerInner { servers: clients })),
            retry_count: DEFAULT_RETRY_COUNT,
            retry_delay: DEFAULT_RETRY_DELAY,
        }
    }

    /// Create a lock manager backed by one Redis Cluster.
    ///
    /// The URIs are seed nodes for the same logical cluster, so the cluster counts as one server
    /// when calculating quorum. Enable the `cluster` crate feature to use this constructor.
    #[cfg(feature = "cluster")]
    pub fn new_cluster<T: IntoConnectionInfo>(uris: Vec<T>) -> RedisResult<LockManager> {
        ClusterClient::new(uris).map(Self::from_cluster_client)
    }

    /// Create a lock manager from a configured Redis Cluster client.
    ///
    /// Use this constructor when the cluster needs authentication, TLS, address remapping, or
    /// other options exposed by [`redis::cluster::ClusterClientBuilder`].
    #[cfg(feature = "cluster")]
    pub fn from_cluster_client(client: ClusterClient) -> LockManager {
        LockManager {
            lock_manager_inner: Arc::new(Mutex::new(LockManagerInner {
                servers: vec![RestorableConnection::new_cluster(client)],
            })),
            retry_count: DEFAULT_RETRY_COUNT,
            retry_delay: DEFAULT_RETRY_DELAY,
        }
    }

    /// Get 20 random bytes from the pseudorandom interface.
    pub fn get_unique_lock_id(&self) -> io::Result<Vec<u8>> {
        let mut buf = [0u8; 20];
        rng().fill_bytes(&mut buf);
        Ok(buf.to_vec())
    }

    /// Set retry count and retry delay.
    ///
    /// Retries will be delayed by a random amount of time between `0` and `retry_delay`.
    ///
    /// Retry count defaults to `3`.
    /// Retry delay defaults to `200`.
    pub fn set_retry(&mut self, count: u32, delay: Duration) {
        self.retry_count = count;
        self.retry_delay = delay;
    }

    async fn lock_inner(&self) -> MutexGuard<'_, LockManagerInner> {
        self.lock_manager_inner.lock().await
    }

    // Can be used for creating or extending a lock
    async fn exec_or_retry(
        &self,
        resource: impl ToLockResource<'_>,
        value: &[u8],
        ttl: usize,
        function: Operation,
    ) -> Result<Lock, LockError> {
        let mut current_try = 1;
        let resource = &resource.to_lock_resource();

        loop {
            let start_time = Instant::now();
            let l = self.lock_inner().await;
            let mut servers = l.servers.clone();
            drop(l);

            let n = match function {
                Operation::Lock => {
                    join_all(servers.iter_mut().map(|c| c.lock(resource, value, ttl))).await
                }
                Operation::Extend => {
                    join_all(servers.iter_mut().map(|c| c.extend(resource, value, ttl))).await
                }
            }
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

            let l = self.lock_inner().await;
            if n >= l.get_quorum() && validity_time > 0 {
                return Ok(Lock {
                    lock_manager: self.clone(),
                    resource: resource.to_vec(),
                    val: value.to_vec(),
                    validity_time,
                });
            }

            let mut servers = l.servers.clone();
            drop(l);
            join_all(
                servers
                    .iter_mut()
                    .map(|client| client.unlock(resource, value)),
            )
            .await;

            // only sleep here if we have any retries left
            if current_try < self.retry_count {
                current_try += 1;

                let retry_delay: u64 = self
                    .retry_delay
                    .as_millis()
                    .try_into()
                    .map_err(|_| LockError::TtlTooLarge)?;

                let n = rng().random_range(0..retry_delay);

                tokio::time::sleep(Duration::from_millis(n)).await
            } else {
                break;
            }
        }

        Err(LockError::Unavailable)
    }

    // Query Redis for a key's value and keep trying each server until a successful result is returned
    pub async fn query_redis_for_key_value(
        &self,
        resource: &[u8],
    ) -> Result<Option<Vec<u8>>, LockError> {
        let l = self.lock_inner().await;
        let mut servers = l.servers.clone();
        drop(l);
        let results = join_all(servers.iter_mut().map(|c| c.query(resource))).await;

        if let Some(value) = results.into_iter().find_map(Result::ok) {
            return Ok(value);
        }
        Err(LockError::RedisConnectionFailed) // All servers failed
    }

    /// Unlock the given lock.
    ///
    /// Unlock is best effort. It will simply try to contact all instances
    /// and remove the key.
    pub async fn unlock(&self, lock: &Lock) {
        let l = self.lock_inner().await;
        let mut servers = l.servers.clone();
        drop(l);
        join_all(
            servers
                .iter_mut()
                .map(|client| client.unlock(&*lock.resource, &lock.val)),
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
    pub async fn lock(
        &self,
        resource: impl ToLockResource<'_>,
        ttl: Duration,
    ) -> Result<Lock, LockError> {
        let resource = resource.to_lock_resource();
        let val = self.get_unique_lock_id().map_err(LockError::Io)?;
        let ttl = ttl
            .as_millis()
            .try_into()
            .map_err(|_| LockError::TtlTooLarge)?;

        self.exec_or_retry(&resource, &val.clone(), ttl, Operation::Lock)
            .await
    }

    /// Loops until the lock is acquired.
    ///
    /// The lock is placed in a guard that will unlock the lock when the guard is dropped.
    ///
    /// May return `LockError::TtlTooLarge` if `ttl` is too large.
    #[cfg(feature = "async-std-comp")]
    pub async fn acquire(
        &self,
        resource: impl ToLockResource<'_>,
        ttl: Duration,
    ) -> Result<LockGuard, LockError> {
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
        resource: impl ToLockResource<'_>,
        ttl: Duration,
    ) -> Result<Lock, LockError> {
        let resource = &resource.to_lock_resource();
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

        self.exec_or_retry(&*lock.resource, &lock.val, ttl, Operation::Extend)
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

    #[cfg(feature = "tokio-comp")]
    pub async fn using<R>(
        &self,
        resource: &[u8],
        ttl: Duration,
        routine: impl AsyncFnOnce() -> R,
    ) -> Result<R, LockError> {
        let mut lock = self.acquire_no_guard(resource, ttl).await?;
        let mut threshold = lock.validity_time as u64 - 500;

        let routine = routine();
        futures::pin_mut!(routine);

        loop {
            match tokio::time::timeout(Duration::from_millis(threshold), &mut routine).await {
                Ok(result) => {
                    self.unlock(&lock).await;

                    return Ok(result);
                }

                Err(_) => {
                    lock = self.extend(&lock, ttl).await?;
                    threshold = lock.validity_time as u64 - 500;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    #[cfg(feature = "cluster")]
    use redis::cluster::{ClusterClientBuilder, NodeAddress};
    #[cfg(feature = "cluster")]
    use std::collections::HashMap;
    #[cfg(feature = "cluster")]
    use testcontainers::ImageExt;
    use testcontainers::{
        core::{IntoContainerPort, WaitFor},
        runners::AsyncRunner,
        ContainerAsync, GenericImage,
    };
    use tokio::time::Duration;

    use super::*;

    type Containers = Vec<ContainerAsync<GenericImage>>;

    async fn create_clients() -> (Containers, Vec<String>) {
        let mut containers = Vec::new();
        let mut addresses = Vec::new();

        for _ in 1..=3 {
            let container = GenericImage::new("redis", "7")
                .with_exposed_port(6379.tcp())
                .with_wait_for(WaitFor::message_on_stdout("Ready to accept connections"))
                .start()
                .await
                .expect("Failed to start Redis container");

            let port = container
                .get_host_port_ipv4(6379)
                .await
                .expect("Failed to get port");
            let address = format!("redis://localhost:{}", port);

            containers.push(container);
            addresses.push(address);
        }

        // Ensure all Redis instances are ready
        ensure_redis_readiness(&addresses)
            .await
            .expect("Redis instances are not ready");

        (containers, addresses)
    }

    /// This function connects to each Redis instance and sends a `PING` command to verify its readiness.
    /// If any Redis instance fails to respond, it retries up to 120 times with a 1000ms delay between attempts.
    /// If readiness is not achieved after the retries, an error is returned.
    ///
    /// # Purpose
    /// This function is particularly useful in CI environments and automated testing to ensure
    /// that Redis containers or instances are fully initialized before running tests. This helps
    /// prevent flaky tests caused by race conditions where Redis is not yet ready.
    async fn ensure_redis_readiness(
        addresses: &[String],
    ) -> Result<(), Box<dyn std::error::Error>> {
        for address in addresses {
            let client = Client::open(address.as_str())?;
            let mut retries = 120;

            while retries > 0 {
                match client.get_multiplexed_async_connection().await {
                    Ok(mut con) => match redis::cmd("PING").query_async::<String>(&mut con).await {
                        Ok(response) => {
                            eprintln!("Redis {} is ready: {}", address, response);
                            break; // Move to the next address
                        }
                        Err(e) => {
                            eprintln!("Redis {} is not ready: {:?}", address, e);
                        }
                    },
                    Err(e) => eprintln!("Failed to connect to Redis {}: {:?}", address, e),
                }

                // Decrement retries and wait before the next attempt
                retries -= 1;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }

            if retries == 0 {
                return Err(format!("Redis {} did not become ready after retries", address).into());
            }
        }

        Ok(())
    }

    fn is_normal<T: Sized + Send + Sync + Unpin>() {}

    // Test that the LockManager is Send + Sync
    #[test]
    fn test_is_normal() {
        is_normal::<LockManager>();
        is_normal::<LockError>();
        is_normal::<Lock>();
        is_normal::<LockGuard>();
    }

    #[cfg(feature = "cluster")]
    #[test]
    fn test_new_cluster_validates_seed_nodes() {
        assert!(LockManager::new_cluster(vec!["not-a-redis-uri"]).is_err());
    }

    #[cfg(feature = "cluster")]
    #[tokio::test]
    async fn test_cluster_lock_lifecycle() -> Result<()> {
        const CLUSTER_PORTS: [u16; 6] = [7000, 7001, 7002, 7003, 7004, 7005];

        let mut image =
            GenericImage::new("grokzen/redis-cluster", "7.2.5").with_wait_for(WaitFor::seconds(10));

        for port in CLUSTER_PORTS {
            image = image.with_exposed_port(port.tcp());
        }

        let container = image
            .with_env_var("IP", "127.0.0.1")
            .with_env_var("LANG", "C.UTF-8")
            .with_env_var("LC_ALL", "C.UTF-8")
            .start()
            .await
            .expect("Failed to start Redis Cluster container");
        let host = container.get_host().await?.to_string();
        let mut address_map = HashMap::new();

        for port in CLUSTER_PORTS {
            address_map.insert(
                NodeAddress::new("127.0.0.1", port),
                NodeAddress::new(host.clone(), container.get_host_port_ipv4(port).await?),
            );
        }

        let seed_port = container.get_host_port_ipv4(CLUSTER_PORTS[0]).await?;
        let seed = format!("redis://{host}:{seed_port}/");
        let client = ClusterClientBuilder::new([seed])
            .node_address_map(address_map)
            .build()?;
        let manager = LockManager::from_cluster_client(client);
        let mut inner = manager.lock_inner().await;
        if let Err(error) = inner.servers[0].get_connection().await {
            eprintln!(
                "Redis Cluster stdout:\n{}",
                String::from_utf8_lossy(&container.stdout_to_vec().await?)
            );
            eprintln!(
                "Redis Cluster stderr:\n{}",
                String::from_utf8_lossy(&container.stderr_to_vec().await?)
            );
            return Err(error.into());
        }
        drop(inner);

        let lock = manager
            .lock("cluster-lock-lifecycle", Duration::from_secs(10))
            .await?;
        assert_eq!(
            manager.query_redis_for_key_value(&lock.resource).await?,
            Some(lock.val.clone())
        );

        let extended = manager.extend(&lock, Duration::from_secs(10)).await?;
        assert!(extended.validity_time > 0);

        manager.unlock(&extended).await;
        assert_eq!(
            manager.query_redis_for_key_value(&lock.resource).await?,
            None
        );

        drop(container);
        Ok(())
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
        let (_containers, addresses) = create_clients().await;

        let rl = LockManager::new(addresses.clone());
        let l = rl.lock_inner().await;

        assert_eq!(3, l.servers.len());
        assert_eq!(2, l.get_quorum());
    }

    #[tokio::test]
    async fn test_lock_direct_unlock_fails() -> Result<()> {
        let (_containers, addresses) = create_clients().await;

        let rl = LockManager::new(addresses.clone());
        let key = rl.get_unique_lock_id()?;

        let val = rl.get_unique_lock_id()?;
        let mut l = rl.lock_inner().await;
        assert!(!l.servers[0].unlock(&key, &val).await);

        Ok(())
    }

    #[tokio::test]
    async fn test_lock_direct_unlock_succeeds() -> Result<()> {
        let (_containers, addresses) = create_clients().await;

        let rl = LockManager::new(addresses.clone());
        let key = rl.get_unique_lock_id()?;

        let val = rl.get_unique_lock_id()?;
        let mut l = rl.lock_inner().await;
        let mut con = l.servers[0].get_connection().await?;

        redis::cmd("SET")
            .arg(&*key)
            .arg(&*val)
            .exec_async(&mut con)
            .await?;

        assert!(l.servers[0].unlock(&key, &val).await);
        Ok(())
    }

    #[tokio::test]
    async fn test_lock_direct_lock_succeeds() -> Result<()> {
        let (_containers, addresses) = create_clients().await;

        let rl = LockManager::new(addresses.clone());
        let key = rl.get_unique_lock_id()?;
        let resource = key.to_lock_resource();

        let val = rl.get_unique_lock_id()?;
        let mut l = rl.lock_inner().await;
        let mut con = l.servers[0].get_connection().await?;

        redis::cmd("DEL").arg(&*key).exec_async(&mut con).await?;
        assert!(l.servers[0].lock(&resource, &val, 10_000).await);
        Ok(())
    }

    #[tokio::test]
    async fn test_lock_unlock() -> Result<()> {
        let (_containers, addresses) = create_clients().await;

        let rl = LockManager::new(addresses.clone());
        let key = rl.get_unique_lock_id()?;

        let val = rl.get_unique_lock_id()?;
        let mut l = rl.lock_inner().await;
        let mut con = l.servers[0].get_connection().await?;
        drop(l);
        let _: () = redis::cmd("SET")
            .arg(&*key)
            .arg(&*val)
            .query_async(&mut con)
            .await?;

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
        let (_containers, addresses) = create_clients().await;

        let rl = LockManager::new(addresses.clone());

        let key = rl.get_unique_lock_id()?;
        match rl.lock(&key, Duration::from_millis(10_000)).await {
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
        let (_containers, addresses) = create_clients().await;

        let rl = LockManager::new(addresses.clone());
        let rl2 = LockManager::new(addresses.clone());

        let key = rl.get_unique_lock_id()?;

        let lock = rl.lock(&key, Duration::from_millis(10_000)).await.unwrap();
        assert!(
            lock.validity_time > 0,
            "validity time: {}",
            lock.validity_time
        );

        if let Ok(_l) = rl2.lock(&key, Duration::from_millis(10_000)).await {
            panic!("Lock acquired, even though it should be locked")
        }

        rl.unlock(&lock).await;

        match rl2.lock(&key, Duration::from_millis(10_000)).await {
            Ok(l) => assert!(l.validity_time > 0),
            Err(_) => panic!("Lock couldn't be acquired"),
        }

        Ok(())
    }

    #[cfg(all(not(feature = "tokio-comp"), feature = "async-std-comp"))]
    #[tokio::test]
    async fn test_lock_lock_unlock_raii() -> Result<()> {
        let (_containers, addresses) = create_clients().await;

        let rl = LockManager::new(addresses.clone());
        let rl2 = LockManager::new(addresses.clone());
        let key = rl.get_unique_lock_id()?;

        async {
            let lock_guard = rl
                .acquire(&key, Duration::from_millis(10_000))
                .await
                .unwrap();
            let lock = &lock_guard.lock;
            assert!(
                lock.validity_time > 0,
                "validity time: {}",
                lock.validity_time
            );

            if let Ok(_l) = rl2.lock(&key, Duration::from_millis(10_000)).await {
                panic!("Lock acquired, even though it should be locked")
            }
        }
        .await;

        match rl2.lock(&key, Duration::from_millis(10_000)).await {
            Ok(l) => assert!(l.validity_time > 0),
            Err(_) => panic!("Lock couldn't be acquired"),
        }

        Ok(())
    }

    #[cfg(feature = "tokio-comp")]
    #[tokio::test]
    async fn test_lock_raii_does_not_unlock_with_tokio_enabled() -> Result<()> {
        let (_containers, addresses) = create_clients().await;

        let rl1 = LockManager::new(addresses.clone());
        let rl2 = LockManager::new(addresses.clone());
        let key = rl1.get_unique_lock_id()?;

        async {
            //The acquire function is only enabled for `async-std-comp` ??
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

            // Retry verifying the Redis key state up to 5 times with a 1000ms delay
            let mut retries = 5;
            let mut redis_key_verified = false;

            while retries > 0 {
                match rl1.query_redis_for_key_value(&key).await {
                    Ok(Some(redis_val)) if redis_val == lock.val => {
                        redis_key_verified = true;
                        break;
                    }
                    Ok(Some(redis_val)) => {
                        println!(
                            "Redis key value mismatch. Expected: {:?}, Found: {:?}. Retrying...",
                            lock.val, redis_val
                        );
                    }
                    Ok(None) => println!("Redis key not found. Retrying..."),
                    Err(e) => println!("Failed to query Redis key: {:?}. Retrying...", e),
                }

                retries -= 1;
                tokio::time::sleep(Duration::from_millis(1000)).await;
            }

            // Acquire lock2 and assert it can't be acquired
            if let Ok(_l) = rl2.lock(&key, Duration::from_millis(10_000)).await {
                panic!("Lock acquired, even though it should be locked")
            }

            assert!(redis_key_verified);
        }
        .await;

        if rl2.lock(&key, Duration::from_millis(10_000)).await.is_ok() {
            panic!("Lock couldn't be acquired");
        }

        Ok(())
    }

    #[cfg(feature = "async-std-comp")]
    #[tokio::test]
    async fn test_lock_extend_lock() -> Result<()> {
        let (_containers, addresses) = create_clients().await;

        let rl1 = LockManager::new(addresses.clone());
        let rl2 = LockManager::new(addresses.clone());

        let key = rl1.get_unique_lock_id()?;

        async {
            let lock1 = rl1
                .acquire(&key, Duration::from_millis(10_000))
                .await
                .unwrap();

            // Wait half a second before locking again
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            rl1.extend(&lock1.lock, Duration::from_millis(10_000))
                .await
                .unwrap();

            // Wait another half a second to see if lock2 can unlock
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            // Assert lock2 can't access after extended lock
            match rl2.lock(&key, Duration::from_millis(10_000)).await {
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
        let (_containers, addresses) = create_clients().await;

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
            if rl2.lock(&key, Duration::from_millis(10_000)).await.is_err() {
                panic!("Unexpected error when trying to claim free lock after extend expired")
            }

            // Also assert rl1 can't reuse lock1
            match rl1.extend(&lock1.lock, Duration::from_millis(10_000)).await {
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
        let (_containers, addresses) = create_clients().await;

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
        let (_containers, addresses) = create_clients().await;
        let rl = LockManager::new(addresses.clone());
        let key = rl.get_unique_lock_id().unwrap();

        // Too big Duration, fails - technical limit is from_millis(u64::MAX)
        let ttl = Duration::from_secs(u64::MAX);
        if rl.lock(&key, ttl).await.is_ok() {
            panic!("Expected LockError::TtlTooLarge");
        }
    }

    #[tokio::test]
    #[cfg(feature = "tokio-comp")]
    async fn test_lock_send_lock_manager() {
        let (_containers, addresses) = create_clients().await;
        let rl = LockManager::new(addresses.clone());

        let lock = rl
            .lock(b"resource", std::time::Duration::from_millis(10_000))
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
    #[cfg(feature = "tokio-comp")]
    async fn test_lock_state_in_multiple_threads() {
        let (_containers, addresses) = create_clients().await;
        let rl = LockManager::new(addresses.clone());

        let lock1 = rl
            .lock(b"resource_1", std::time::Duration::from_millis(10_000))
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
            Err(LockError::RedisKeyNotFound) => {}
            Err(e) => panic!("Unexpected error: {:?}", e),
        };

        let lock2 = rl
            .lock(b"resource_2", std::time::Duration::from_millis(10_000))
            .await
            .unwrap();
        rl.unlock(&lock2).await;

        match rl.is_freed(&lock2).await {
            Ok(freed) => assert!(freed, "Lock should be freed after unlock"),
            Err(LockError::RedisKeyNotFound) => {}
            Err(e) => panic!("Unexpected error: {:?}", e),
        };
    }

    #[tokio::test]
    async fn test_redis_value_matches_lock_value() {
        let (_containers, addresses) = create_clients().await;
        let rl = LockManager::new(addresses.clone());

        let lock = rl
            .lock(b"resource_1", std::time::Duration::from_millis(10_000))
            .await
            .unwrap();

        // Ensure Redis key is correctly set and matches the lock value
        let mut l = rl.lock_inner().await;
        let mut con = l.servers[0].get_connection().await.unwrap();
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
        let (_containers, addresses) = create_clients().await;
        let rl = LockManager::new(addresses.clone());

        let lock = rl
            .lock(b"resource_1", std::time::Duration::from_millis(10_000))
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
        let (_containers, addresses) = create_clients().await;
        let rl = LockManager::new(addresses.clone());

        let lock = rl
            .lock(b"resource_2", std::time::Duration::from_millis(10_000))
            .await
            .unwrap();

        rl.unlock(&lock).await;

        match rl.is_freed(&lock).await {
            Ok(freed) => assert!(freed, "Lock should be freed after unlock"),
            Err(LockError::RedisKeyNotFound) => {}
            Err(e) => panic!("Unexpected error: {:?}", e),
        };
    }

    #[tokio::test]
    async fn test_is_freed_when_key_missing_in_redis() {
        let (_containers, addresses) = create_clients().await;
        let rl = LockManager::new(addresses.clone());

        let lock = rl
            .lock(b"resource_3", std::time::Duration::from_millis(10_000))
            .await
            .unwrap();

        // Manually delete the key in Redis to simulate it being missing
        let mut l = rl.lock_inner().await;
        let mut con = l.servers[0].get_connection().await.unwrap();
        drop(l);

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
            Err(LockError::RedisKeyNotFound) => {}
            Err(e) => panic!("Unexpected error: {:?}", e),
        };
    }

    #[tokio::test]
    async fn test_is_freed_handles_redis_connection_failure() {
        let (_containers, _) = create_clients().await;
        let rl = LockManager::new(Vec::<String>::new()); // No Redis clients, simulate failure

        let lock_result = rl
            .lock(b"resource_4", std::time::Duration::from_millis(10_000))
            .await;

        match lock_result {
            Ok(lock) => {
                // Since there are no clients, any check with Redis will fail
                match rl.is_freed(&lock).await {
                    Ok(freed) => panic!("Expected failure due to Redis connection, but got Ok with freed status: {}", freed),
                    Err(LockError::RedisConnectionFailed) => {}
                    Err(e) => panic!("Unexpected error: {:?}", e),
                }
            }
            Err(LockError::Unavailable) => {}
            Err(e) => panic!("Unexpected error while acquiring lock: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_redis_connection_failed() {
        let (_containers, _) = create_clients().await;
        let rl = LockManager::new(Vec::<String>::new()); // No Redis clients, simulate failure

        let lock_result = rl
            .lock(b"resource_5", std::time::Duration::from_millis(10_000))
            .await;

        match lock_result {
            Ok(lock) => match rl.is_freed(&lock).await {
                Err(LockError::RedisConnectionFailed) => {}
                Ok(_) => panic!("Expected RedisConnectionFailed, but got Ok"),
                Err(e) => panic!("Unexpected error: {:?}", e),
            },
            Err(LockError::Unavailable) => {}
            Err(e) => panic!("Unexpected error while acquiring lock: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_redis_key_mismatch() {
        let (_containers, addresses) = create_clients().await;
        let rl = LockManager::new(addresses.clone());

        let lock = rl
            .lock(b"resource_6", std::time::Duration::from_millis(10_000))
            .await
            .unwrap();

        // Set a different value for the same key to simulate a mismatch
        let mut l = rl.lock_inner().await;
        let mut con = l.servers[0].get_connection().await.unwrap();
        drop(l);
        let different_value: Vec<u8> = vec![1, 2, 3, 4, 5]; // Different value
        redis::cmd("SET")
            .arg(&lock.resource)
            .arg(different_value)
            .query_async::<()>(&mut con)
            .await
            .unwrap();

        // Now check if is_freed identifies the mismatch correctly
        match rl.is_freed(&lock).await {
            Err(LockError::RedisKeyMismatch) => {}
            Ok(_) => panic!("Expected RedisKeyMismatch, but got Ok"),
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_redis_key_not_found() {
        let (_containers, addresses) = create_clients().await;
        let rl = LockManager::new(addresses.clone());

        let lock = rl
            .lock(b"resource_7", std::time::Duration::from_millis(10_000))
            .await
            .unwrap();

        // Manually delete the key in Redis to simulate it being missing
        let mut l = rl.lock_inner().await;
        let mut con = l.servers[0].get_connection().await.unwrap();
        drop(l);
        redis::cmd("DEL")
            .arg(&lock.resource)
            .query_async::<()>(&mut con)
            .await
            .unwrap();

        match rl.is_freed(&lock).await {
            Err(LockError::RedisKeyNotFound) => {}
            Ok(_) => panic!("Expected RedisKeyNotFound, but got Ok"),
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_lock_manager_from_clients_valid_instance() {
        let (_containers, addresses) = create_clients().await;

        let clients: Vec<Client> = addresses
            .iter()
            .map(|uri| Client::open(uri.as_str()).unwrap())
            .collect();

        let lock_manager = LockManager::from_clients(clients);

        let l = lock_manager.lock_inner().await;
        assert_eq!(l.servers.len(), 3);
        assert_eq!(l.get_quorum(), 2);
    }

    #[tokio::test]
    async fn test_lock_manager_from_clients_partial_quorum() {
        let (_containers, addresses) = create_clients().await;
        let mut clients: Vec<Client> = addresses
            .iter()
            .map(|uri| Client::open(uri.as_str()).unwrap())
            .collect();

        // Remove one client to simulate fewer nodes
        clients.pop();

        let lock_manager = LockManager::from_clients(clients);

        let l = lock_manager.lock_inner().await;
        assert_eq!(l.servers.len(), 2);
        assert_eq!(l.get_quorum(), 2); // 2/2+1 still rounds to 2
    }
}
