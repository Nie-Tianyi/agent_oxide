use std::future::Future;

use futures_util::stream::BoxStream;

use crate::error::ProviderError;
use crate::request::CompletionRequest;
use crate::response::CompletionResponse;
use crate::stream::StreamChunk;

/// Abstraction over an LLM provider.
///
/// Implementations handle provider-specific HTTP details, authentication,
/// and wire-protocol parsing. The rest of the agent framework only depends
/// on this trait — never on a concrete provider type.
///
/// Uses Rust 2024 native async traits with explicit `Send` bounds so
/// the futures can be spawned across tokio tasks.
pub trait LLMClient: Send + Sync {
    /// Send a non-streaming completion request.
    fn generate(
        &self,
        req: CompletionRequest,
    ) -> impl Future<Output = Result<CompletionResponse, ProviderError>> + Send;

    /// Send a streaming completion request.
    ///
    /// Returns a [`BoxStream`] that yields chunks as they arrive.
    fn stream(
        &self,
        req: CompletionRequest,
    ) -> impl Future<
        Output = Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError>,
    > + Send;
}
