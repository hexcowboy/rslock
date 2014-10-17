# redlock-rs - Distributed locks with Redis

This is an implementation of Redlock, the [distributed locking mechanism][distlock] built on top of Redis.
It is more or less a port of the [Ruby version][redlock.rb].

It currently lacks documentation. I write that ASAP.  
It includes a sample application in [main.rs](src/main.rs).

## Build

```
cargo build --release
```

## Usage

```rust
use redlock::RedLock;

fn main() {
  let rl = RedLock::new(vec!["redis://127.0.0.1:6380/", "redis://127.0.0.1:6381/", "redis://127.0.0.1:6382/"]);

  let lock;
  loop {
    match rl.lock("mutex".as_bytes(), 1000) {
      Some(l) => { lock = l; break }
      None => ()
    }
  }

  // Critical section

  rl.unlock(&lock);
}

```

## Tests

Run tests with:

```
cargo test
```

Run sample application with:

```
./target/release/redlock
```

## Contribute

If you find bugs or want to help otherwise, please [open an issue](https://github.com/badboy/redlock-rs/issues).

## License

BSD. See [LICENSE](LICENSE).  

[distlock]: http://redis.io/topics/distlock
[redlock.rb]: https://github.com/antirez/redlock-rb
