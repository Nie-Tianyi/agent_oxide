//! # Memory — conversation memory management
//!
//! In-memory conversation buffer with compaction and disk persistence.

pub mod memory;
pub mod persistence;

pub use memory::{
    COMPACTED_TOOL_OUTPUT_PLACEHOLDER, CompactSignal, DEFAULT_COMPACT_CHARS,
    DEFAULT_COMPACTABLE_TOOLS, DEFAULT_KEEP_LAST_N, DEFAULT_KEEP_RECENT_TOOL_OUTPUTS, Memory,
    MemoryBuilder, MemoryError, SharedMemory,
};
pub use persistence::{
    ThreadInfo, default_thread_name, generate_thread_name, list_threads, load_conversation,
    read_current_thread, save_conversation, write_current_thread,
};
