#[cfg(any(feature = "smol-comp", feature = "tokio-comp"))]
mod resource;

#[cfg(any(feature = "smol-comp", feature = "tokio-comp"))]
mod lock;

#[cfg(any(feature = "smol-comp", feature = "tokio-comp"))]
pub use crate::lock::{Lock, LockError, LockGuard, LockManager};
