use redis;
use redis::{RedisResult, Value, Nil, Okay};
use std::io::{File, IoResult};
use std::io::timer::sleep;
use std::time::duration::Duration;
use std::rand;
use std::rand::distributions::{IndependentSample, Range};
use time;

const DEFAULT_RETRY_COUNT : int = 3;
const DEFAULT_RETRY_DELAY : int = 200;
const CLOCK_DRIFT_FACTOR : f32 = 0.01;
const UNLOCK_SCRIPT : &'static str = r"if redis.call('get',KEYS[1]) == ARGV[1] then
                                        return redis.call('del',KEYS[1])
                                      else
                                        return 0
                                      end";

pub struct RedLock {
    pub servers: Vec<redis::Client>,
    quorum: uint,
    retry_count: int,
    retry_delay: int
}

pub struct Lock<'a> {
    pub resource: &'a [u8],
    pub val: Vec<u8>,
    pub validity_time: uint
}

impl RedLock {
    pub fn new(uris: Vec<&str>) -> RedLock {
        let quorum = uris.len() / 2 + 1;
        let mut servers = Vec::with_capacity(uris.len());

        for &uri in uris.iter() {
            servers.push(redis::Client::open(uri).unwrap())
        }

        RedLock {
            servers: servers,
            quorum: quorum,
            retry_count: DEFAULT_RETRY_COUNT,
            retry_delay: DEFAULT_RETRY_DELAY
        }
    }

    pub fn get_unique_lock_id(&self) -> IoResult<Vec<u8>> {
        let mut file = File::open(&Path::new("/dev/urandom"));
        file.read_exact(20)
    }

    pub fn set_retry(&mut self, count: int, delay: int) {
        self.retry_count = count;
        self.retry_delay = delay;
    }

    fn lock_instance(&self, client: &redis::Client, resource: &[u8], val: &[u8], ttl: uint) -> bool {
        let con = match client.get_connection() {
            Err(_) => return false,
            Ok(val) => val
        };
        let result : RedisResult<Value> = redis::cmd("SET").arg(resource).arg(val).arg("nx").arg("px").arg(ttl).query(&con);
        match result {
            Ok(Okay) => return true,
            Ok(Nil)  => return false,
            Ok(_)    => return false,
            Err(_)   => return false
        }
    }

    fn get_time(&self) -> i64 {
        let time = time::get_time();

        time.sec * 1000 + ((time.nsec/1000000) as i64)
    }

    pub fn lock<'a>(&'a self, resource: &'a [u8], ttl: uint) -> Option<Lock> {
        let val = self.get_unique_lock_id().unwrap();

        let between = Range::new(0, self.retry_delay);
        let mut rng = rand::task_rng();

        for _ in range(0, self.retry_count) {
            let mut n = 0;
            let start_time = self.get_time();
            for &ref client in self.servers.iter() {
                if self.lock_instance(client, resource, val.as_slice(), ttl) {
                    n += 1;
                }
            }

            let drift = (ttl as f32 * CLOCK_DRIFT_FACTOR) as int + 2;
            let validity_time = (ttl as i64 - ((self.get_time() - start_time)) - drift as i64) as uint;

            if n >= self.quorum && validity_time > 0 {
                return Some(Lock {
                    resource: resource.clone(),
                    val: val,
                    validity_time: validity_time
                });
            } else {
                for &ref client in self.servers.iter() {
                    self.unlock_instance(client, resource, val.as_slice());
                }
            }

            let n = between.ind_sample(&mut rng);
            sleep(Duration::milliseconds(n as i64));
        }
        return None
    }

    fn unlock_instance(&self, client: &redis::Client, resource: &[u8], val: &[u8]) -> bool {
        let con = match client.get_connection() {
            Err(_) => return false,
            Ok(val) => val
        };
        let script = redis::Script::new(UNLOCK_SCRIPT);
        let result : RedisResult<int> = script.key(resource).arg(val).invoke(&con);
        match result {
            Ok(val) => return val == 1,
            Err(_)  => return false
        }
    }

    pub fn unlock(&self, lock: &Lock) {
        for &ref client in self.servers.iter() {
            self.unlock_instance(client, lock.resource, lock.val.as_slice());
        }
    }
}

#[test]
fn test_redlock_get_unique_id() {
    let rl = RedLock::new(vec![]);

    match rl.get_unique_lock_id() {
        Ok(id) => {
            assert_eq!(20, id.len());
        },
        err => fail!("Error thrown: {}", err)
    }
}

#[test]
fn test_redlock_get_unique_id_uniqueness() {
    let rl = RedLock::new(vec![]);

    let id1 = rl.get_unique_lock_id().unwrap();
    let id2 = rl.get_unique_lock_id().unwrap();

    assert_eq!(20, id1.len());
    assert_eq!(20, id2.len());
    assert!(id1 != id2);
}

#[test]
fn test_redlock_valid_instance() {
    let rl = RedLock::new(vec!["redis://127.0.0.1:6380/", "redis://127.0.0.1:6381/", "redis://127.0.0.1:6382/", ]);
    assert_eq!(3, rl.servers.len());
    assert_eq!(2, rl.quorum);
}

#[test]
fn test_redlock_direct_unlock_fails() {
    let rl = RedLock::new(vec!["redis://127.0.0.1:6380/", "redis://127.0.0.1:6381/", "redis://127.0.0.1:6382/", ]);
    let key = rl.get_unique_lock_id().unwrap();

    let val = rl.get_unique_lock_id().unwrap();
    assert_eq!(false, rl.unlock_instance(&rl.servers[0], key[], val[]))
}

#[test]
fn test_redlock_direct_unlock_succeeds() {
    let rl = RedLock::new(vec!["redis://127.0.0.1:6380/", "redis://127.0.0.1:6381/", "redis://127.0.0.1:6382/", ]);
    let key = rl.get_unique_lock_id().unwrap();

    let val = rl.get_unique_lock_id().unwrap();
    let con = rl.servers[0].get_connection().unwrap();
    redis::cmd("SET").arg(key[]).arg(val[]).execute(&con);

    assert_eq!(true, rl.unlock_instance(&rl.servers[0], key[], val[]))
}

#[test]
fn test_redlock_direct_lock_succeeds() {
    let rl = RedLock::new(vec!["redis://127.0.0.1:6380/", "redis://127.0.0.1:6381/", "redis://127.0.0.1:6382/", ]);
    let key = rl.get_unique_lock_id().unwrap();

    let val = rl.get_unique_lock_id().unwrap();
    let con = rl.servers[0].get_connection().unwrap();

    redis::cmd("DEL").arg(key[]).execute(&con);
    assert_eq!(true, rl.lock_instance(&rl.servers[0], key[], val[], 1000))
}

#[test]
fn test_redlock_unlock() {
    let rl = RedLock::new(vec!["redis://127.0.0.1:6380/", "redis://127.0.0.1:6381/", "redis://127.0.0.1:6382/", ]);
    let key = rl.get_unique_lock_id().unwrap();

    let val = rl.get_unique_lock_id().unwrap();
    let con = rl.servers[0].get_connection().unwrap();
    let _ : () = redis::cmd("SET").arg(key[]).arg(val[]).query(&con).unwrap();

    let lock = Lock { resource: key[], val: val, validity_time: 0 };
    assert_eq!((), rl.unlock(&lock))
}

#[test]
fn test_redlock_lock() {
    let rl = RedLock::new(vec!["redis://127.0.0.1:6380/", "redis://127.0.0.1:6381/", "redis://127.0.0.1:6382/", ]);

    let key = rl.get_unique_lock_id().unwrap();
    match rl.lock(key[], 1000) {
        Some(lock) => {
            assert_eq!(key[], lock.resource);
            assert_eq!(20, lock.val.len());
            assert!(lock.validity_time > 900);
        },
        None => fail!("Lock failed")
    }
}

#[test]
fn test_redlock_lock_unlock() {
    let rl = RedLock::new(vec!["redis://127.0.0.1:6380/", "redis://127.0.0.1:6381/", "redis://127.0.0.1:6382/", ]);
    let rl2 = RedLock::new(vec!["redis://127.0.0.1:6380/", "redis://127.0.0.1:6381/", "redis://127.0.0.1:6382/", ]);

    let key = rl.get_unique_lock_id().unwrap();

    let lock = rl.lock(key[], 1000).unwrap();
    assert!(lock.validity_time > 900);

    match rl2.lock(key[], 1000) {
        Some(l) => fail!("Lock acquired, even though it should be locked"),
        None => ()
    }

    rl.unlock(&lock);

    match rl2.lock(key[], 1000) {
        Some(l) => assert!(l.validity_time > 900),
        None => fail!("Lock couldn't be acquired")
    }
}
