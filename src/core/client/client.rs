use reqwest::Client as HttpClient;

use super::error::DeepSeekError;
use super::request::DeepSeekRequest;
use super::response::DeepSeekResponse;
use super::stream::DeepSeekStream;

// ── Client ──────────────────────────────────────────────────────────────────

const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";

pub struct DeepSeekClient {
    api_key: String,
    base_url: String,
    http: HttpClient,
}

impl DeepSeekClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            http: HttpClient::new(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Send a non-streaming chat completion request.
    ///
    /// Returns an error if `request.stream` is `true`. Use [`Self::stream`] instead.
    pub async fn send(&self, request: DeepSeekRequest) -> Result<DeepSeekResponse, DeepSeekError> {
        if request.stream {
            return Err(DeepSeekError::StreamingNotSupported);
        }

        let url = format!("{}/v1/chat/completions", self.base_url);

        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await
            .map_err(DeepSeekError::Http)?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(DeepSeekError::Api {
                status: status.as_u16(),
                body,
            });
        }

        response
            .json::<DeepSeekResponse>()
            .await
            .map_err(|e| DeepSeekError::Parse(e.to_string()))
    }

    /// Send a streaming chat completion request.
    ///
    /// The request's `stream` field is forced to `true`.
    pub async fn stream(
        &self,
        mut request: DeepSeekRequest,
    ) -> Result<DeepSeekStream, DeepSeekError> {
        request.stream = true;

        let url = format!("{}/v1/chat/completions", self.base_url);

        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await
            .map_err(DeepSeekError::Http)?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(DeepSeekError::Api {
                status: status.as_u16(),
                body,
            });
        }

        Ok(DeepSeekStream {
            response,
            buffer: Vec::new(),
            finished: false,
        })
    }
}
