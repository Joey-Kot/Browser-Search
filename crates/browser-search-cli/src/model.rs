use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchKind {
    Web,
    News,
    Images,
    Videos,
    Forums,
}

impl SearchKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::News => "news",
            Self::Images => "images",
            Self::Videos => "videos",
            Self::Forums => "forums",
        }
    }

    pub(crate) const fn is_images(self) -> bool {
        matches!(self, Self::Images)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SearchRequest {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

pub type SearchResult = IndexMap<String, String>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatusResponse {
    pub extension_connected: bool,
    pub max_concurrency: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ErrorEnvelope {
    pub error: ApiErrorDetail,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiErrorDetail {
    pub code: String,
    pub message: String,
    #[allow(dead_code)]
    pub retryable: bool,
}
