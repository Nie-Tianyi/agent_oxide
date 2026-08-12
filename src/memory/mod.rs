//! # Memory — conversation memory management
//!
//! In-memory conversation buffer types. Disk persistence lives in the
//! [`crate::persistence`] module; shared utilities live in [`crate::util`].

mod buffer;

pub use buffer::{Memory, PendingHints, SharedMemory};
