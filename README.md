# rslock - Redlock for Redis in Rust

[![Crates.io](https://img.shields.io/crates/v/rslock)][crates.io]
[![Docs badge]][docs.rs]

This is an implementation of Redlock, the [distributed locking mechanism](http://redis.io/topics/distlock) built on top of Redis.

## Features

- Lock extending
- Async runtime support (async-std and tokio)
- Async redis

## Install

```bash
cargo add rslock
```

> [!NOTE]  
> The `default` feature of this crate will provide `async-std`. You may optionally use `tokio` by supplying the `tokio-comp` feature flag when installing, but `tokio` has limitations that will not grant access to some parts of the API ([read more here](https://github.com/hexcowboy/rslock/pull/4#issuecomment-1693711182)).

## Build

```
cargo build --release
```

## Usage

```rust
use rslock::LockManager;

#[tokio::main]
async fn main() {
    let rl = LockManager::new(vec![
        "redis://127.0.0.1:6380/",
        "redis://127.0.0.1:6381/",
        "redis://127.0.0.1:6382/",
    ]);

    let lock;
    loop {
        // Create the lock
        if let Ok(l) = rl.lock("mutex".as_bytes(), 1000).await {
            lock = l;
            break;
        }
    }

    // Extend the lock
    match rl.extend(&lock, 1000).await {
        Ok(_) => println!("lock extended!"),
        Err(_) => println!("lock couldn't be extended"),
    }

    // Unlock the lock
    rl.unlock(&lock).await;
}
```

## Extending Locks

It should be noted that "extending" locks actually just renews them. For example, when you extend a 1000ms lock after 500ms have elapsed by another 1000ms, the lock will live for a total of 1500ms. It does not add additional time the the existing lock. This is how it was implemented in the Node.js version of Redlock and it will remain that way to be consistent. See the [extend script](https://github.com/hexcowboy/rslock/blob/main/src/lock.rs#L22-L30).

## Tests

Run tests with:

```
cargo test
```

## Contribute

If you find bugs or want to help otherwise, please [open an issue](https://github.com/hexcowboy/rslock/issues).

## License

BSD. See [LICENSE](LICENSE).

[docs badge]: https://img.shields.io/badge/docs.rs-rustdoc-green
[crates.io]: https://crates.io/crates/rslock
[docs.rs]: https://docs.rs/rslock/
