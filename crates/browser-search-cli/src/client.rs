use std::time::Duration;

use reqwest::{Response, StatusCode};
use serde::de::DeserializeOwned;
use url::Url;

use crate::{
    error::CliError,
    model::{ErrorEnvelope, SearchKind, SearchRequest, SearchResult, StatusResponse},
};

const RESPONSE_PREVIEW_MAX: usize = 300;

pub(crate) struct SearchClient {
    base_url: Url,
    api_token: String,
    http: reqwest::Client,
}

impl SearchClient {
    pub(crate) fn new(
        base_url: Url,
        api_token: String,
        timeout: Duration,
    ) -> Result<Self, CliError> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .no_proxy()
            .user_agent(concat!("browser-search-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| {
                CliError::runtime(format!("failed to initialize HTTP client: {error}"))
            })?;
        Ok(Self {
            base_url,
            api_token,
            http,
        })
    }

    pub(crate) async fn status(&self) -> Result<StatusResponse, CliError> {
        let endpoint = self.endpoint("v1/status")?;
        let response = self
            .http
            .get(endpoint)
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map_err(|error| request_error("read daemon status", error))?;
        self.parse_response(response).await
    }

    pub(crate) async fn search(
        &self,
        kind: SearchKind,
        request: &SearchRequest,
    ) -> Result<Vec<SearchResult>, CliError> {
        let endpoint = self.endpoint(&format!("v1/search/{}", kind.as_str()))?;
        let response = self
            .http
            .post(endpoint)
            .bearer_auth(&self.api_token)
            .json(request)
            .send()
            .await
            .map_err(|error| request_error(&format!("search {}", kind.as_str()), error))?;
        self.parse_response(response).await
    }

    async fn parse_response<T>(&self, response: Response) -> Result<T, CliError>
    where
        T: DeserializeOwned,
    {
        let status = response.status();
        let body = response.bytes().await.map_err(|error| {
            CliError::runtime(format!("failed to read API response body: {error}"))
        })?;

        if !status.is_success() {
            if let Ok(envelope) = serde_json::from_slice::<ErrorEnvelope>(&body) {
                return Err(CliError::runtime(format!(
                    "API returned {} ({}): {}",
                    status.as_u16(),
                    envelope.error.code,
                    envelope.error.message
                )));
            }
            let preview = response_preview(&body);
            return Err(CliError::runtime(if preview.is_empty() {
                format!("API returned HTTP {}", status.as_u16())
            } else {
                format!("API returned HTTP {}: {preview}", status.as_u16())
            }));
        }

        if status != StatusCode::OK {
            return Err(CliError::runtime(format!(
                "API returned unexpected success status {}",
                status.as_u16()
            )));
        }

        serde_json::from_slice(&body)
            .map_err(|error| CliError::runtime(format!("API response is invalid JSON: {error}")))
    }

    fn endpoint(&self, path: &str) -> Result<Url, CliError> {
        self.base_url
            .join(path)
            .map_err(|error| CliError::runtime(format!("failed to build API URL: {error}")))
    }
}

fn request_error(action: &str, error: reqwest::Error) -> CliError {
    CliError::runtime(format!("failed to {action}: {error}"))
}

fn response_preview(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut characters = compact.chars();
    let preview = characters
        .by_ref()
        .take(RESPONSE_PREVIEW_MAX)
        .collect::<String>();
    if characters.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}
