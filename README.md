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
cargo add "rslock@~0.10.0" --features tokio-comp
```

Select a runtime explicitly; no runtime is enabled by default:

```bash
cargo add "rslock@~0.10.0" --features tokio-comp
# Or use Smol:
cargo add "rslock@~0.10.0" --features smol-comp
```

Both backends include Rustls TLS support and can be enabled together. `async-std-comp`
is retained as an alias for `smol-comp`; Redis no longer has a separate async-std backend.
When both backends are enabled, Redis uses Tokio inside a Tokio runtime and Smol otherwise.

## Build

```
cargo build --release --features tokio-comp
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
    let guard = rl
        .acquire("my_mutex", Duration::from_millis(1000))
        .await
        .expect("Failed to acquire lock");

    println!("Lock acquired!");

    // Extend the lock
    if rl.extend(&guard.lock, Duration::from_millis(1000)).await.is_ok() {
        println!("Lock extended!");
    } else {
        println!("Failed to extend the lock.");
    }

    // Unlock the lock
    guard.release().await;
    println!("Lock released!");
}
```

More examples:

- [Basic locking](examples/basic.rs)
- [Creating a manager from Redis clients](examples/from_clients.rs)
- [Sharing a lock between tasks](examples/shared_lock.rs)
- [Smol runtime](examples/smol.rs)
- [Using Redis Cluster](examples/cluster.rs)

### Redis Cluster

Enable `cluster` together with a runtime feature and pass one or more seed-node URIs for one logical
cluster. The cluster counts as one backend when `rslock` calculates quorum:

```bash
cargo add "rslock@~0.10.0" --features cluster,tokio-comp
```

For authentication, TLS, address mapping, and other advanced configuration, build a
`redis::cluster::ClusterClient` and pass it to `LockManager::from_cluster_client`.

## Migrating to 0.10

- Choose `tokio-comp` or `smol-comp` explicitly. Defaults are empty, and `cluster`
  requires a runtime selection too. `async-std-comp` remains an alias for `smol-comp`.
- Guards no longer unlock on drop. Call `guard.release().await` before returning,
  including error paths. A panic, cancelled future, or dropped guard leaves the
  Redis key until its TTL expires.
- `acquire`, `acquire_no_guard`, and `using` are available on both runtimes.
  An owned `Lock` also has `release().await`; shared locks can still use
  `manager.unlock(&lock).await`.

Release consumes the lock or guard and returns `()`. It awaits best-effort cleanup
on all backends, just like `unlock`; it does not confirm deletion on unreachable
servers. TTL remains the fallback. Keep work within the lock's validity period,
renewing it as needed.

`using` renews the lock while its routine runs and attempts release when the
routine finishes or renewal fails. For example, on either runtime:

```rust
let result = manager.using(b"resource", Duration::from_secs(10), async || {
    do_work().await
}).await?;
```

Cancellation and panic still rely on TTL expiry. There is no background cleanup.

## Extending Locks

Extending a lock effectively renews its duration instead of adding extra time to it. For instance, if a 1000ms lock is extended by 1000ms after 500ms pass, it will only last for a total of 1500ms, not 2000ms. This approach is consistent with the [Node.js Redlock implementation](https://www.npmjs.com/package/redlock). See the [extend script](https://github.com/hexcowboy/rslock/blob/main/src/lock.rs#L30-L40).

## Tests

The integration tests use Testcontainers and require Docker. Run the all-features
and individual runtime configurations:

```bash
cargo test --all-features --all-targets
cargo test --features tokio-comp --all-targets
cargo test --features smol-comp --all-targets
cargo check --no-default-features --all-targets
```

## Running the examples

Start the standalone Redis servers used by the first three examples:

```bash
docker compose -f examples/docker-compose.yml up -d
```

Run the standalone Redis examples:

```bash
cargo run --example basic --features tokio-comp
cargo run --example shared_lock --features tokio-comp
cargo run --example from_clients --features tokio-comp
cargo run --example smol --features smol-comp
```

Stop the redis servers:

```bash
docker compose -f examples/docker-compose.yml down
```

To run the [Redis Cluster example](examples/cluster.rs), first start a Redis Cluster
with seed nodes on ports 7000–7002, then run:

```bash
cargo run --example cluster --features cluster,tokio-comp
```

## Contribute

If you find bugs or want to help otherwise, please [open an issue](https://github.com/hexcowboy/rslock/issues).

## License

BSD. See [LICENSE](LICENSE).

[docs badge]: https://img.shields.io/badge/docs.rs-rustdoc-green
[crates.io]: https://crates.io/crates/rslock
[docs.rs]: https://docs.rs/rslock/
