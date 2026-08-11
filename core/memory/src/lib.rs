#![deny(unsafe_code)]
//! # Memory — conversation memory management
//!
//! In-memory conversation buffer types. Disk persistence lives in the
//! `persistence` extension crate; shared utilities live in the `util` crate.

pub mod memory;

pub use memory::{Memory, PendingHints, SharedMemory};
