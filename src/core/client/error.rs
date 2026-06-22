#[derive(Debug)]
pub enum DeepSeekError {
    /// reqwest error (connection, timeout, DNS, TLS).
    Http(reqwest::Error),
    /// API returned a non-2xx status.
    Api { status: u16, body: String },
    /// Failed to deserialize the response.
    Parse(String),
    /// Streaming is not supported yet.
    StreamingNotSupported,
}

impl std::fmt::Display for DeepSeekError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(e) => write!(f, "HTTP error: {e}"),
            Self::Api { status, body } => write!(f, "API error ({status}): {body}"),
            Self::Parse(e) => write!(f, "parse error: {e}"),
            Self::StreamingNotSupported => write!(f, "streaming is not yet supported"),
        }
    }
}

impl std::error::Error for DeepSeekError {}
