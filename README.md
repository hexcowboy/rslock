# rslock - RedLock for Redis in Rust

![Crates.io](https://img.shields.io/crates/v/rslock)
[![Docs badge]][docs.rs]

This is an implementation of Redlock, the [distributed locking mechanism][distlock] built on top of Redis.

It is a fork of the existing [redlock-rs](https://github.com/badboy/redlock-rs) **with additional features like async Redis and the RedLock extend feature**.

## Build

```
cargo build --release
```

## Usage

```rust
use redlock::RedLock;

#[tokio::main]
async fn main() {
    let rl = RedLock::new(vec![
        "redis://127.0.0.1:6380/",
        "redis://127.0.0.1:6381/",
        "redis://127.0.0.1:6382/",
    ]);

    let lock;
    loop {
        // Create the lock
        match rl.lock("mutex".as_bytes(), 1000).await {
            Ok(l) => {
                lock = l;
                break;
            }
            Err(_) => (),
        }
    }

    // Extend the lock
    match rl.extend(&lock, 1000).await {
        Ok(_) => println!("lock extended!"),
        Err(_) => (),
    }

    // Unlock the lock
    rl.unlock(&lock).await;
}
```

## Extending Locks

It should be noted that "extending" redlocks actually just renews them. For example, when you extend a 1000ms lock after 500ms have elapsed by another 1000ms, the lock will live for a total of 1500ms. It does not add additional time the the existing lock. This is how it was implemented in the Node.js version of redlock and it will remain that way to be consistent. See the [extend script](https://github.com/hexcowboy/rslock/blob/main/src/redlock.rs#L22-L30).

## Tests

Run tests with:

```
cargo test
```

Run sample application with:

```
cargo run --release
```

## Contribute

If you find bugs or want to help otherwise, please [open an issue](https://github.com/rsecob/redlock-async-rs/issues).

## License

BSD. See [LICENSE](LICENSE).

[distlock]: http://redis.io/topics/distlock
[docs badge]: https://img.shields.io/badge/docs.rs-rustdoc-green
[docs.rs]: https://docs.rs/rslock/
