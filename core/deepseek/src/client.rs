use futures_util::stream::BoxStream;
use reqwest::Client as HttpClient;

use provider::{CompletionRequest, CompletionResponse, LLMClient, ProviderError, StreamChunk};

use crate::error::DeepSeekError;
use crate::request::DeepSeekRequest;
use crate::response::DeepSeekResponse;
use crate::stream::DeepSeekStream;

const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";

/// DeepSeek API client — implements [`LLMClient`].
#[derive(Clone)]
pub struct DeepSeekClient {
    api_key: String,
    base_url: String,
    http: HttpClient,
}

impl DeepSeekClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        let base_url = std::env::var("DEEPSEEK_BASE_URL")
            .or_else(|_| std::env::var("BASE_URL"))
            .unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned());
        Self {
            api_key: api_key.into(),
            base_url,
            http: HttpClient::new(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Check the HTTP response status and return an `Api` error on failure.
    /// Returns the response back on success so the caller can parse the body.
    async fn check_response_status(
        response: reqwest::Response,
    ) -> Result<reqwest::Response, DeepSeekError> {
        let status = response.status();
        if !status.is_success() {
            let url = response.url().to_string();
            let body = response.text().await.unwrap_or_default();
            tracing::warn!(
                status = status.as_u16(),
                url = %url,
                body = %body.chars().take(500).collect::<String>(),
                "DeepSeek API returned error status",
            );
            return Err(DeepSeekError::Api {
                status: status.as_u16(),
                body,
            });
        }
        Ok(response)
    }

    /// Send a non-streaming completion request.
    pub async fn complete(
        &self,
        request: DeepSeekRequest,
    ) -> Result<DeepSeekResponse, DeepSeekError> {
        if request.stream {
            return Err(DeepSeekError::StreamNotSupported);
        }

        let url = format!("{}/v1/chat/completions", self.base_url);
        tracing::debug!(
            model = %request.model,
            message_count = request.messages.len(),
            tool_count = request.tools.as_ref().map_or(0, Vec::len),
            "sending non-streaming completion request",
        );
        let start = std::time::Instant::now();
        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await
            .map_err(DeepSeekError::Http)?;

        let response = Self::check_response_status(response).await?;

        let ds_resp = response
            .json::<DeepSeekResponse>()
            .await
            .map_err(|e| DeepSeekError::Parse(e.to_string()))?;
        tracing::debug!(
            latency_ms = start.elapsed().as_millis() as u64,
            total_tokens = ds_resp.usage.as_ref().map_or(0, |u| u.total_tokens),
            "completion request finished",
        );
        Ok(ds_resp)
    }

    /// Start a streaming completion request.
    pub async fn start_stream(
        &self,
        mut request: DeepSeekRequest,
    ) -> Result<DeepSeekStream, DeepSeekError> {
        request.stream = true;

        let url = format!("{}/v1/chat/completions", self.base_url);
        tracing::debug!(
            model = %request.model,
            message_count = request.messages.len(),
            tool_count = request.tools.as_ref().map_or(0, Vec::len),
            "opening streaming completion request",
        );
        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await
            .map_err(DeepSeekError::Http)?;

        let response = Self::check_response_status(response).await?;

        Ok(DeepSeekStream {
            response,
            buffer: Vec::new(),
            finished: false,
        })
    }
}

impl LLMClient for DeepSeekClient {
    async fn generate(&self, req: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        let ds_req = DeepSeekRequest::from(req);
        let ds_resp = self.complete(ds_req).await?;
        Ok(CompletionResponse::from(ds_resp))
    }

    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
        let ds_req = DeepSeekRequest::from(req);
        let ds_stream = self.start_stream(ds_req).await?;

        let stream = futures_util::stream::try_unfold(ds_stream, |mut stream| async move {
            match stream.next().await {
                Some(Ok(chunk)) => Ok(Some((chunk, stream))),
                Some(Err(e)) => Err(ProviderError::from(e)),
                None => Ok(None),
            }
        });

        Ok(Box::pin(stream))
    }
}
