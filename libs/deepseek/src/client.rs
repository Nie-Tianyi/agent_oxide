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
        let base_url = std::env::var("BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned());
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

    /// Send a non-streaming request (DeepSeek-specific API).
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

    /// Send a streaming request (DeepSeek-specific API).
    pub async fn stream_raw(
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

impl LLMClient for DeepSeekClient {
    async fn generate(&self, req: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        let ds_req = DeepSeekRequest::from(req);
        let ds_resp = self.send(ds_req).await?;
        Ok(CompletionResponse::from(ds_resp))
    }

    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
        let ds_req = DeepSeekRequest::from(req);
        let ds_stream = self.stream_raw(ds_req).await?;

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
