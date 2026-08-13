use thiserror::Error;

use crate::provider::ProviderError;

/// DeepSeek-specific error.
///
/// Conversion into [`ProviderError`] preserves the source chain —
/// `DeepSeekError::Http(reqwest::Error)` becomes
/// `ProviderError::Http(Box<dyn Error>)` instead of a flattened string.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DeepSeekError {
    /// Network / transport error.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    /// API returned a non-2xx status.
    #[error("API error ({status}): {body}")]
    Api { status: u16, body: String },
    /// Failed to parse the response (the message may embed a snippet of
    /// the offending data).
    #[error("parse error: {0}")]
    Parse(String),
    /// Streaming is not supported by this provider.
    #[error("streaming is not supported")]
    StreamNotSupported,
}

impl From<DeepSeekError> for ProviderError {
    fn from(e: DeepSeekError) -> Self {
        match e {
            // Box the transport error so the source chain survives the
            // provider boundary: AgentError → ProviderError → reqwest::Error.
            DeepSeekError::Http(err) => ProviderError::Http(Box::new(err)),
            DeepSeekError::Api { status, body } => ProviderError::Api { status, body },
            DeepSeekError::Parse(msg) => ProviderError::Parse(msg),
            DeepSeekError::StreamNotSupported => ProviderError::StreamingNotSupported,
        }
    }
}
