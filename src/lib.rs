#![crate_name = "redlock"]
#![crate_type = "lib"]
#![license = "BSD"]
#![comment = "Distributed Lock on top of Redis"]

#![feature(slicing_syntax)]
#![experimental]

extern crate redis;
extern crate time;

pub use redlock::{RedLock, Lock};
mod redlock;
