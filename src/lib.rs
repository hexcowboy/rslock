mod lock;

#[cfg(any(feature = "async-std-comp", feature = "tokio-comp"))]
pub use crate::lock::{Lock, LockError, LockManager};
#[cfg(all(feature = "async-std-comp", not(feature = "tokio-comp")))]
pub use crate::lock::LockGuard;
