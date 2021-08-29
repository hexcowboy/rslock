use std::fs::File;
use std::io::{self, Read};
use std::thread::sleep;
use std::time::{Duration, Instant};

use rand::{thread_rng, Rng};
use redis::Value::Okay;
use redis::{Client, IntoConnectionInfo, RedisResult, Value};

const DEFAULT_RETRY_COUNT: u32 = 3;
const DEFAULT_RETRY_DELAY: u32 = 200;
const CLOCK_DRIFT_FACTOR: f32 = 0.01;
const UNLOCK_SCRIPT: &str = r"if redis.call('get',KEYS[1]) == ARGV[1] then
                                return redis.call('del',KEYS[1])
                              else
                                return 0
                              end";

/// The lock manager.
///
/// Implements the necessary functionality to acquire and release locks
/// and handles the Redis connections.
#[derive(Debug, Clone)]
pub struct RedLock {
    /// List of all Redis clients
    pub servers: Vec<Client>,
    quorum: u32,
    retry_count: u32,
    retry_delay: u32,
}

pub struct Lock<'a> {
    /// The resource to lock. Will be used as the key in Redis.
    pub resource: Vec<u8>,
    /// The value for this lock.
    pub val: Vec<u8>,
    /// Time the lock is still valid.
    /// Should only be slightly smaller than the requested TTL.
    pub validity_time: usize,
    /// Used to limit the lifetime of a lock to its lock manager.
    pub lock_manager: &'a RedLock,
}

pub struct RedLockGuard<'a> {
    pub lock: Lock<'a>,
}

impl Drop for RedLockGuard<'_> {
    fn drop(&mut self) {
        self.lock.lock_manager.unlock(&self.lock);
    }
}

impl RedLock {
    /// Create a new lock manager instance, defined by the given Redis connection uris.
    /// Quorum is defined to be N/2+1, with N being the number of given Redis instances.
    ///
    /// Sample URI: `"redis://127.0.0.1:6379"`
    pub fn new<T: AsRef<str> + IntoConnectionInfo>(uris: Vec<T>) -> RedLock {
        let quorum = (uris.len() as u32) / 2 + 1;

        let servers: Vec<Client> = uris
            .into_iter()
            .map(|uri| Client::open(uri).unwrap())
            .collect();

        RedLock {
            servers,
            quorum,
            retry_count: DEFAULT_RETRY_COUNT,
            retry_delay: DEFAULT_RETRY_DELAY,
        }
    }

    /// Get 20 random bytes from `/dev/urandom`.
    pub fn get_unique_lock_id(&self) -> io::Result<Vec<u8>> {
        let file = File::open("/dev/urandom")?;
        let mut buf = Vec::with_capacity(20);
        match file.take(20).read_to_end(&mut buf) {
            Ok(20) => Ok(buf),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::Other,
                "Can't read enough random bytes",
            )),
            Err(e) => Err(e),
        }
    }

    /// Set retry count and retry delay.
    ///
    /// Retry count defaults to `3`.
    /// Retry delay defaults to `200`.
    pub fn set_retry(&mut self, count: u32, delay: u32) {
        self.retry_count = count;
        self.retry_delay = delay;
    }

    fn lock_instance(
        &self,
        client: &redis::Client,
        resource: &[u8],
        val: &[u8],
        ttl: usize,
    ) -> bool {
        let mut con = match client.get_connection() {
            Err(_) => return false,
            Ok(val) => val,
        };
        let result: RedisResult<Value> = redis::cmd("SET")
            .arg(resource)
            .arg(val)
            .arg("nx")
            .arg("px")
            .arg(ttl)
            .query(&mut con);
        match result {
            Ok(Okay) => true,
            Ok(_) | Err(_) => false,
        }
    }

    /// Acquire the lock for the given resource and the requested TTL.
    ///
    /// If it succeeds, a `Lock` instance is returned,
    /// including the value and the validity time
    ///
    /// If it fails. `None` is returned.
    /// A user should retry after a short wait time.
    pub fn lock(&self, resource: &[u8], ttl: usize) -> Option<Lock> {
        let val = self.get_unique_lock_id().unwrap();

        let mut rng = thread_rng();

        for _ in 0..self.retry_count {
            let mut n = 0;
            let start_time = Instant::now();
            for client in &self.servers {
                if self.lock_instance(client, resource, &val, ttl) {
                    n += 1;
                }
            }

            let drift = (ttl as f32 * CLOCK_DRIFT_FACTOR) as usize + 2;
            let elapsed = start_time.elapsed();
            let validity_time = ttl
                - drift
                - elapsed.as_secs() as usize * 1000
                - elapsed.subsec_nanos() as usize / 1_000_000;

            if n >= self.quorum && validity_time > 0 {
                return Some(Lock {
                    lock_manager: self,
                    resource: resource.to_vec(),
                    val,
                    validity_time,
                });
            } else {
                for client in &self.servers {
                    self.unlock_instance(client, resource, &val);
                }
            }

            let n = rng.gen_range(0..self.retry_delay);
            sleep(Duration::from_millis(u64::from(n)));
        }
        None
    }

    /// Acquire the lock for the given resource and the requested TTL. \
    /// Will wait and yield current task (tokio runtime) until the lock \
    /// is acquired
    ///
    /// Returns a `RedLockGuard` instance which is a RAII wrapper for \
    /// the old `Lock` object
    #[cfg(feature = "async")]
    pub async fn acquire_async(&self, resource: &[u8], ttl: usize) -> RedLockGuard<'_> {
        let lock;
        loop {
            match self.lock(resource, ttl) {
                Some(l) => {
                    lock = l;
                    break;
                }
                None => tokio::task::yield_now().await,
            }
        }
        RedLockGuard { lock }
    }

    pub fn acquire(&self, resource: &[u8], ttl: usize) -> RedLockGuard<'_> {
        let lock;
        loop {
            if let Some(l) = self.lock(resource, ttl) {
                lock = l;
                break;
            }
        }
        RedLockGuard { lock }
    }

    fn unlock_instance(&self, client: &redis::Client, resource: &[u8], val: &[u8]) -> bool {
        let mut con = match client.get_connection() {
            Err(_) => return false,
            Ok(val) => val,
        };
        let script = redis::Script::new(UNLOCK_SCRIPT);
        let result: RedisResult<i32> = script.key(resource).arg(val).invoke(&mut con);
        match result {
            Ok(val) => val == 1,
            Err(_) => false,
        }
    }

    /// Unlock the given lock.
    ///
    /// Unlock is best effort. It will simply try to contact all instances
    /// and remove the key.
    pub fn unlock(&self, lock: &Lock) {
        for client in &self.servers {
            self.unlock_instance(client, &lock.resource, &lock.val);
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use once_cell::sync::Lazy;
    use testcontainers::clients::Cli;
    use testcontainers::images::redis::Redis;
    use testcontainers::{Container, Docker};

    use super::*;

    static DOCKER: Lazy<Cli> = Lazy::new(Cli::default);
    static CONTAINERS: Lazy<Vec<Container<Cli, Redis>>> = Lazy::new(|| {
        (0..3)
            .map(|_| DOCKER.run(Redis::default().with_tag("6-alpine")))
            .collect()
    });
    static ADDRESSES: Lazy<Vec<String>> = Lazy::new(|| match std::env::var("ADDRESSES") {
        Ok(addresses) => addresses.split(',').map(String::from).collect(),
        Err(_) => CONTAINERS
            .iter()
            .map(|c| format!("redis://localhost:{}", c.get_host_port(6379).unwrap()))
            .collect(),
    });

    #[test]
    fn test_redlock_get_unique_id() -> Result<()> {
        let rl = RedLock::new(Vec::<String>::new());
        assert_eq!(rl.get_unique_lock_id()?.len(), 20);
        Ok(())
    }

    #[test]
    fn test_redlock_get_unique_id_uniqueness() -> Result<()> {
        let rl = RedLock::new(Vec::<String>::new());

        let id1 = rl.get_unique_lock_id()?;
        let id2 = rl.get_unique_lock_id()?;

        assert_eq!(20, id1.len());
        assert_eq!(20, id2.len());
        assert_ne!(id1, id2);
        Ok(())
    }

    #[test]
    fn test_redlock_valid_instance() {
        println!("{}", ADDRESSES.join(","));
        let rl = RedLock::new(ADDRESSES.clone());
        assert_eq!(3, rl.servers.len());
        assert_eq!(2, rl.quorum);
    }

    #[test]
    fn test_redlock_direct_unlock_fails() -> Result<()> {
        println!("{}", ADDRESSES.join(","));
        let rl = RedLock::new(ADDRESSES.clone());
        let key = rl.get_unique_lock_id()?;

        let val = rl.get_unique_lock_id()?;
        assert!(!rl.unlock_instance(&rl.servers[0], &key, &val));
        Ok(())
    }

    #[test]
    fn test_redlock_direct_unlock_succeeds() -> Result<()> {
        println!("{}", ADDRESSES.join(","));
        let rl = RedLock::new(ADDRESSES.clone());
        let key = rl.get_unique_lock_id()?;

        let val = rl.get_unique_lock_id()?;
        let mut con = rl.servers[0].get_connection()?;
        redis::cmd("SET").arg(&*key).arg(&*val).execute(&mut con);

        assert!(rl.unlock_instance(&rl.servers[0], &key, &val));
        Ok(())
    }

    #[test]
    fn test_redlock_direct_lock_succeeds() -> Result<()> {
        println!("{}", ADDRESSES.join(","));
        let rl = RedLock::new(ADDRESSES.clone());
        let key = rl.get_unique_lock_id()?;

        let val = rl.get_unique_lock_id()?;
        let mut con = rl.servers[0].get_connection()?;

        redis::cmd("DEL").arg(&*key).execute(&mut con);
        assert!(rl.lock_instance(&rl.servers[0], &*key, &*val, 1000));
        Ok(())
    }

    #[test]
    fn test_redlock_unlock() -> Result<()> {
        println!("{}", ADDRESSES.join(","));
        let rl = RedLock::new(ADDRESSES.clone());
        let key = rl.get_unique_lock_id()?;

        let val = rl.get_unique_lock_id()?;
        let mut con = rl.servers[0].get_connection()?;
        let _: () = redis::cmd("SET")
            .arg(&*key)
            .arg(&*val)
            .query(&mut con)
            .unwrap();

        let lock = Lock {
            lock_manager: &rl,
            resource: key,
            val,
            validity_time: 0,
        };
        rl.unlock(&lock);
        Ok(())
    }

    #[test]
    fn test_redlock_lock() -> Result<()> {
        println!("{}", ADDRESSES.join(","));
        let rl = RedLock::new(ADDRESSES.clone());

        let key = rl.get_unique_lock_id()?;
        match rl.lock(&key, 1000) {
            Some(lock) => {
                assert_eq!(key, lock.resource);
                assert_eq!(20, lock.val.len());
                assert!(lock.validity_time > 900);
                assert!(
                    lock.validity_time > 900,
                    "validity time: {}",
                    lock.validity_time
                );
            }
            None => panic!("Lock failed"),
        }
        Ok(())
    }

    #[test]
    fn test_redlock_lock_unlock() -> Result<()> {
        println!("{}", ADDRESSES.join(","));
        let rl = RedLock::new(ADDRESSES.clone());
        let rl2 = RedLock::new(ADDRESSES.clone());

        let key = rl.get_unique_lock_id()?;

        let lock = rl.lock(&key, 1000).unwrap();
        assert!(
            lock.validity_time > 900,
            "validity time: {}",
            lock.validity_time
        );

        if let Some(_l) = rl2.lock(&key, 1000) {
            panic!("Lock acquired, even though it should be locked")
        }

        rl.unlock(&lock);

        match rl2.lock(&key, 1000) {
            Some(l) => assert!(l.validity_time > 900),
            None => panic!("Lock couldn't be acquired"),
        }
        Ok(())
    }

    #[test]
    fn test_redlock_lock_unlock_raii() -> Result<()> {
        println!("{}", ADDRESSES.join(","));
        let rl = RedLock::new(ADDRESSES.clone());
        let rl2 = RedLock::new(ADDRESSES.clone());

        let key = rl.get_unique_lock_id()?;
        {
            let lock_guard = rl.acquire(&key, 1000);
            let lock = &lock_guard.lock;
            assert!(
                lock.validity_time > 900,
                "validity time: {}",
                lock.validity_time
            );

            if let Some(_l) = rl2.lock(&key, 1000) {
                panic!("Lock acquired, even though it should be locked")
            }
        }

        match rl2.lock(&key, 1000) {
            Some(l) => assert!(l.validity_time > 900),
            None => panic!("Lock couldn't be acquired"),
        }
        Ok(())
    }
}
