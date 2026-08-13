use thiserror::Error;

/// Provider-agnostic error type for LLM API interactions.
///
/// Errors preserve their source chain: `AgentError::Provider` →
/// `ProviderError::Http` → the underlying transport error (e.g.
/// `reqwest::Error`), so callers can trace back to the root cause
/// with [`std::error::Error::source`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProviderError {
    /// Network / transport error (connection, TLS, timeout, …).
    #[error("provider HTTP error: {0}")]
    Http(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// API returned a non-2xx status.
    #[error("provider API error (status {status}): {body}")]
    Api { status: u16, body: String },
    /// Failed to parse the response.
    #[error("provider parse error: {0}")]
    Parse(String),
    /// Streaming is not supported by this provider.
    #[error("provider does not support streaming")]
    StreamingNotSupported,
}

impl ProviderError {
    /// Construct an [`Http`](Self::Http) error from a plain message
    /// (synthesized errors and tests — prefer boxing the original error
    /// when one is available, to keep the source chain).
    pub fn http_message(msg: impl Into<String>) -> Self {
        #[derive(Debug)]
        struct MessageError(String);

        impl std::fmt::Display for MessageError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl std::error::Error for MessageError {}

        Self::Http(Box::new(MessageError(msg.into())))
    }
}
