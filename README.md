# rslock - Redlock for Redis in Rust

[![Crates.io](https://img.shields.io/crates/v/rslock)][crates.io]
[![Docs badge]][docs.rs]

This is an implementation of Redlock, the [distributed locking mechanism](https://redis.io/docs/latest/develop/clients/patterns/distributed-locks/) built on top of Redis.

## Features

- Lock extending
- Smol and Tokio Redis I/O backends
- Async Redis
- Redis Cluster support

## Install

> [!WARNING]
> Before release `1.0.0`, this crate will have breaking changes between minor versions. You can upgrade to patch versions without worrying about breaking changes.

```bash
# It is recommended to pin the version to a minor release, as breaking changes may be introduced between minor versions before 1.0.0.
cargo add rslock --vers "~0.9.1"
```

> [!NOTE]
> The default `async-std-comp` feature uses Redis's Smol backend with Rustls. The feature
> name is retained for compatibility. For standalone Redis, you can select Redis's Tokio
> backend instead by disabling the default features:
>
> ```bash
> cargo add rslock --vers "~0.9.1" --no-default-features --features tokio-comp
> ```

## Build

```
cargo build --release
```

## Usage

The example below uses Tokio. Select the `tokio-comp` backend as shown above and add
Tokio to your application:

```bash
cargo add tokio --features macros,rt-multi-thread
```

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

    // Initialize the LockManager using `new`
    let rl = LockManager::new(uris);

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

More examples:

- [Basic locking](examples/basic.rs)
- [Creating a manager from Redis clients](examples/from_clients.rs)
- [Sharing a lock between tasks](examples/shared_lock.rs)
- [Using Redis Cluster](examples/cluster.rs)

### Redis Cluster

Enable the `cluster` feature and pass one or more seed-node URIs for one logical
cluster. The cluster counts as one backend when `rslock` calculates quorum:

```bash
cargo add rslock --vers "~0.9.1" --features cluster
```

For authentication, TLS, address mapping, and other advanced configuration, build a
`redis::cluster::ClusterClient` and pass it to `LockManager::from_cluster_client`.

## Extending Locks

Extending a lock effectively renews its duration instead of adding extra time to it. For instance, if a 1000ms lock is extended by 1000ms after 500ms pass, it will only last for a total of 1500ms, not 2000ms. This approach is consistent with the [Node.js Redlock implementation](https://www.npmjs.com/package/redlock). See the [extend script](https://github.com/hexcowboy/rslock/blob/main/src/lock.rs#L30-L40).

## Tests

The integration tests use Testcontainers and require Docker. Run both the all-features
and default-feature test configurations:

```bash
cargo test --all-features --all-targets
cargo test --all-targets
```

## Running the examples

Start the standalone Redis servers used by the first three examples:

```bash
docker compose -f examples/docker-compose.yml up -d
```

Run the standalone Redis examples:

```bash
cargo run --example basic
cargo run --example shared_lock
cargo run --example from_clients
```

Stop the redis servers:

```bash
docker compose -f examples/docker-compose.yml down
```

To run the [Redis Cluster example](examples/cluster.rs), first start a Redis Cluster
with seed nodes on ports 7000–7002, then run:

```bash
cargo run --example cluster --features cluster
```

## Contribute

If you find bugs or want to help otherwise, please [open an issue](https://github.com/hexcowboy/rslock/issues).

## License

BSD. See [LICENSE](LICENSE).

[docs badge]: https://img.shields.io/badge/docs.rs-rustdoc-green
[crates.io]: https://crates.io/crates/rslock
[docs.rs]: https://docs.rs/rslock/
