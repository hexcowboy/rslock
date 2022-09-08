# redlock-async-rs - Async Distributed locks with Redis

![GitHub Workflow Status](https://img.shields.io/github/workflow/status/rsecob/redlock-async-rs/CI)
![Crates.io](https://img.shields.io/crates/v/redlock-async)

This is an implementation of Redlock, the [distributed locking mechanism][distlock] built on top of Redis.

It is a fork of existing [redlock-rs](https://github.com/badboy/redlock-rs) with async built on top of it.

## Build

```
cargo build --release
```

## Usage

```rust
use redlock::RedLock;

#[tokio::main]
fn main() {
  let rl = RedLock::new(vec!["redis://127.0.0.1:6380/", "redis://127.0.0.1:6381/", "redis://127.0.0.1:6382/"]);

  let lock;
  loop {
    match rl.lock("mutex".as_bytes(), 1000).await {
      Ok(l) => { lock = l; break }
      Err(_) => ()
    }
  }

  // Critical section
  rl.unlock(&lock).await;
}

```

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
[redlock.rb]: https://github.com/antirez/redlock-rb
[redlock-rs]: https://github.com/badboy/redlock-rs
