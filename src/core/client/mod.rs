mod client;
mod error;
mod request;
mod response;
mod stream;

// Re-export everything so external callers see a flat `client::*` namespace.
pub use client::DeepSeekClient;
pub use error::DeepSeekError;
pub use request::{
    DeepSeekRequest, FunctionDef, Message, ReasoningEffort, ResponseFormat, ResponseFormatType,
    Role, Thinking, ThinkingType, ToolCall, ToolCallFunction, ToolCallType, ToolChoice,
    ToolChoiceFunction, ToolDef, ToolDefType,
};
pub use response::{Choice, ChoiceMessage, DeepSeekResponse, FinishReason, Usage};
pub use stream::{ChunkChoice, DeepSeekChunk, DeepSeekStream, Delta};
