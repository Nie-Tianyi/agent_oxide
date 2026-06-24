//! # Agent Oxide — Async Rust Agent Framework
//!
//! This is the library crate. The thin binary in `main.rs` uses it.
//!
//! ## Modules
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`core`] | Agent framework core — DeepSeek client, agent loop |
//! | [`memory`] | Conversation history with two-phase async compaction |

pub mod core;
pub mod memory;
