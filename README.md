# rslock - Redlock for Redis in Rust

[![Crates.io](https://img.shields.io/crates/v/rslock)][crates.io]
[![Docs badge]][docs.rs]

This is an implementation of Redlock, the [distributed locking mechanism](http://redis.io/topics/distlock) built on top of Redis.

## Features

- Lock extending
- Async runtime support (async-std and tokio)
- Async redis
- Support for both standalone Redis and Redis Cluster

## Install

> [!WARNING]
> Before release `1.0.0`, this crate will have breaking changes between minor versions. You can upgrade to patch versions without worrying about breaking changes.

```bash
# It is recommended to pin the version to a minor release, as breaking changes may be introduced between minor versions before 1.0.0.
cargo add rslock --vers "~0.7.2"
```

> [!NOTE]
> The `default` feature of this crate will provide `async-std`. You may optionally use tokio by supplying the `tokio-comp` feature flag when installing.

## Build

```
cargo build --release
```

## Usage

```rust
use rslock::LockManager;
use std::time::Duration;

#[tokio::main]
async fn main() {
    // Define Redis URIs
    let uris = vec![
        "redis://127.0.0.1:6380/",
        "redis://127.0.0.1:6381/",
        "redis://127.0.0.1:6382/",
    ];

    // Initialize the LockManager using `new` for standalone Redis
    let rl = LockManager::new(uris);

    // For Redis Cluster, use:
    // let cluster_uris = vec![
    //     vec!["redis://127.0.0.1:7000/", "redis://127.0.0.1:7001/"],
    //     vec!["redis://127.0.0.1:7002/", "redis://127.0.0.1:7003/"],
    // ];
    // let rl = LockManager::new_cluster(cluster_uris)?;

    // Acquire a lock
    let lock = loop {
        if let Ok(lock) = rl
            .lock("my_mutex", Duration::from_millis(1000))
            .await
        {
            break lock;
        }
    };

    println!("Lock acquired!");

    // Extend the lock
    if rl.extend(&lock, Duration::from_millis(1000)).await.is_ok() {
        println!("Lock extended!");
    } else {
        println!("Failed to extend the lock.");
    }

    // Unlock the lock
    rl.unlock(&lock).await;
    println!("Lock released!");
}
```

## Locking Behavior

- **Single cluster**: Simple Redis lock (non-distributed, no quorum)
- **Multiple clusters**: Distributed Redlock (quorum-based, N≥3)

## Extending Locks

Extending a lock effectively renews its duration instead of adding extra time to it. For instance, if a 1000ms lock is extended by 1000ms after 500ms pass, it will only last for a total of 1500ms, not 2000ms. This approach is consistent with the [Node.js Redlock implementation](https://www.npmjs.com/package/redlock). See the [extend script](https://github.com/hexcowboy/rslock/blob/main/src/lock.rs#L22-L30).

## Tests

Make sure you have Docker running since all tests use `testcontainers`. Run tests with:

```
cargo test --all-features
```

## Examples

### Basic Examples

Start the redis servers:

```bash
docker compose -f examples/docker-compose.yml up -d
```

Run the examples:

```bash
cargo run --example basic
cargo run --example shared_lock
cargo run --example from_clients
```

Stop the redis servers:

```bash
docker compose -f examples/docker-compose.yml down
```

### Cluster Examples

Test single-cluster (simple lock) and multi-cluster (Redlock) behavior:

```bash
# Executes both single and multi-cluster examples
docker compose -f examples/docker-compose-cluster.yml up --build
```

## Contribute

If you find bugs or want to help otherwise, please [open an issue](https://github.com/hexcowboy/rslock/issues).

## License

BSD. See [LICENSE](LICENSE).

[docs badge]: https://img.shields.io/badge/docs.rs-rustdoc-green
[crates.io]: https://crates.io/crates/rslock
[docs.rs]: https://docs.rs/rslock/
