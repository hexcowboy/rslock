# rslock - Redlock for Redis in Rust

[![Crates.io](https://img.shields.io/crates/v/rslock)][crates.io]
[![Docs badge]][docs.rs]

This is an implementation of Redlock, the [distributed locking mechanism](http://redis.io/topics/distlock) built on top of Redis.

## Features

- Lock extending
- Async runtime support (async-std and tokio)
- Async redis

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

## Extending Locks

Extending a lock effectively renews its duration instead of adding extra time to it. For instance, if a 1000ms lock is extended by 1000ms after 500ms pass, it will only last for a total of 1500ms, not 2000ms. This approach is consistent with the [Node.js Redlock implementation](https://www.npmjs.com/package/redlock). See the [extend script](https://github.com/hexcowboy/rslock/blob/main/src/lock.rs#L22-L30).

## Tests

Make sure you have Docker running since all tests use `testcontainers`. Run tests with:

```
cargo test --all-features
```

## Examples

Start the redis servers mentioned in the example code:

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

## Contribute

If you find bugs or want to help otherwise, please [open an issue](https://github.com/hexcowboy/rslock/issues).

## License

BSD. See [LICENSE](LICENSE).

[docs badge]: https://img.shields.io/badge/docs.rs-rustdoc-green
[crates.io]: https://crates.io/crates/rslock
[docs.rs]: https://docs.rs/rslock/
