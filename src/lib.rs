#[cfg(any(feature = "async-std-comp", feature = "tokio-comp"))]
mod lock;

#[cfg(any(feature = "async-std-comp", feature = "tokio-comp"))]
pub use crate::lock::{Lock, LockError, LockGuard, LockManager};
