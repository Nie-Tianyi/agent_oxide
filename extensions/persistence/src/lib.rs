#![deny(unsafe_code)]
//! # Persistence — conversation thread storage
//!
//! Disk I/O for saving and loading conversation threads, thread naming
//! utilities, and an auto-save agent hook.

pub mod hook;
pub mod persistence;

pub use hook::PersistenceHook;
pub use persistence::{
    PersistenceConfig, ThreadInfo, default_thread_name, list_threads, load_conversation,
    read_current_thread_name, sanitize_filename, save_conversation, thread_name_from_message,
    write_current_thread_name,
};
