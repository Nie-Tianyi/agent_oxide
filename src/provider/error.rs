/// Provider-agnostic error type for LLM API interactions.
#[derive(Debug)]
#[non_exhaustive]
pub enum ProviderError {
    /// Network / transport error.
    Http { message: String },
    /// API returned a non-2xx status.
    Api { status: u16, body: String },
    /// Failed to parse the response.
    Parse { message: String },
    /// Streaming is not supported by this provider.
    StreamingNotSupported,
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http { message } => write!(f, "HTTP error: {message}"),
            Self::Api { status, body } => write!(f, "API error ({status}): {body}"),
            Self::Parse { message } => write!(f, "parse error: {message}"),
            Self::StreamingNotSupported => write!(f, "streaming is not supported"),
        }
    }
}

impl std::error::Error for ProviderError {}
