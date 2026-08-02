use std::{collections::BTreeMap, fmt, str::FromStr};

use serde::{
    Deserialize, Serialize, Serializer,
    ser::{SerializeMap, SerializeSeq},
};
use uuid::Uuid;

use crate::{
    config::{Config, FieldTransform},
    error::{ErrorCode, ErrorDetail},
};

fn default_limit() -> u32 {
    10
}

const DEFAULT_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchKind {
    Web,
    News,
    Images,
    Videos,
    Forums,
}

impl SearchKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::News => "news",
            Self::Images => "images",
            Self::Videos => "videos",
            Self::Forums => "forums",
        }
    }

    const fn result_field_order(self) -> &'static [&'static str] {
        match self {
            Self::Web => &["title", "description", "url"],
            Self::News => &["title", "description", "url", "source", "time"],
            Self::Images => &["title", "imgurl", "url"],
            Self::Videos => &["title", "description", "url", "duration"],
            Self::Forums => &["title", "description", "url"],
        }
    }
}

impl fmt::Display for SearchKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SearchKind {
    type Err = ErrorDetail;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "web" => Ok(Self::Web),
            "news" => Ok(Self::News),
            "images" => Ok(Self::Images),
            "videos" => Ok(Self::Videos),
            "forums" => Ok(Self::Forums),
            _ => Err(ErrorDetail::invalid_request(
                "搜索类型必须是 web、news、images、videos 或 forums",
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default)]
    pub start: u32,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

impl SearchRequest {
    pub(crate) fn normalized(
        mut self,
        config: &Config,
    ) -> Result<NormalizedSearchRequest, ErrorDetail> {
        self.query = self.query.trim().to_owned();
        if self.query.is_empty() {
            return Err(ErrorDetail::invalid_request("query 不能为空"));
        }
        if self.query.chars().count() > 512 {
            return Err(ErrorDetail::invalid_request("query 不能超过 512 个字符"));
        }
        if self.start > 1_000 {
            return Err(ErrorDetail::invalid_request("start 不能超过 1000"));
        }
        if !(1..=100).contains(&self.limit) {
            return Err(ErrorDetail::invalid_request("limit 必须在 1 到 100 之间"));
        }
        let timeout_ms = self
            .timeout_ms
            .unwrap_or(DEFAULT_TIMEOUT_MS.min(config.executor.max_timeout_ms));
        if timeout_ms < 1_000 || timeout_ms > config.executor.max_timeout_ms {
            return Err(ErrorDetail::invalid_request(format!(
                "timeoutMs 必须在 1000 到 {} 之间",
                config.executor.max_timeout_ms
            )));
        }
        Ok(NormalizedSearchRequest {
            query: self.query,
            start: self.start,
            limit: self.limit,
            timeout_ms,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NormalizedSearchRequest {
    pub(crate) query: String,
    pub(crate) start: u32,
    pub(crate) limit: u32,
    pub(crate) timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchCommand {
    pub kind: SearchKind,
    pub query: String,
    pub start: u32,
    pub limit: u32,
    pub timeout_ms: u64,
    pub load_timeout_ms: u64,
    pub selector_timeout_ms: u64,
    pub url: String,
    pub extraction: ExtractionRules,
}

impl SearchCommand {
    pub(crate) fn new(
        kind: SearchKind,
        request: NormalizedSearchRequest,
        config: &Config,
    ) -> Result<Self, ErrorDetail> {
        let profile = config.search.profile(kind);
        let limit = profile.limit.unwrap_or(request.limit);
        let mut url = url::Url::parse(&config.search.common.base_url).map_err(|error| {
            ErrorDetail::new(
                ErrorCode::InternalError,
                format!("搜索 base_url 无效: {error}"),
                false,
            )
        })?;
        {
            let mut params = config.search.common.params.clone();
            params.extend(profile.params.clone());
            let mut query = url.query_pairs_mut();
            for (name, value) in &params {
                query.append_pair(name, value);
            }
            query.append_pair(&config.search.common.query_parameter, &request.query);
            if kind != SearchKind::Images && !config.search.common.start_parameter.is_empty() {
                query.append_pair(
                    &config.search.common.start_parameter,
                    &request.start.to_string(),
                );
            }
            if !config.search.common.limit_parameter.is_empty() {
                query.append_pair(&config.search.common.limit_parameter, &limit.to_string());
            }
        }

        Ok(Self {
            kind,
            query: request.query,
            start: request.start,
            limit,
            timeout_ms: request.timeout_ms,
            load_timeout_ms: config.executor.load_timeout_ms.min(request.timeout_ms),
            selector_timeout_ms: config.executor.selector_timeout_ms.min(request.timeout_ms),
            url: url.to_string(),
            extraction: ExtractionRules {
                root_selectors: profile.root_selectors.clone(),
                dedupe_field: profile.dedupe_field.clone(),
                fields: profile
                    .fields
                    .iter()
                    .filter(|(_, field)| field.enabled)
                    .map(|(name, field)| {
                        (
                            name.clone(),
                            ExtractionFieldRule {
                                selectors: field.selectors.clone(),
                                attribute: field.attribute.clone(),
                                transform: field.transform,
                                required: field.required,
                                max_length: field.max_length,
                            },
                        )
                    })
                    .collect(),
            },
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionRules {
    pub root_selectors: Vec<String>,
    pub dedupe_field: String,
    pub fields: BTreeMap<String, ExtractionFieldRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionFieldRule {
    pub selectors: Vec<String>,
    pub attribute: Option<String>,
    pub transform: FieldTransform,
    pub required: bool,
    pub max_length: Option<usize>,
}

pub type SearchResult = BTreeMap<String, String>;

pub(crate) struct SearchResponse {
    kind: SearchKind,
    results: Vec<SearchResult>,
}

impl SearchResponse {
    pub(crate) const fn new(kind: SearchKind, results: Vec<SearchResult>) -> Self {
        Self { kind, results }
    }
}

impl Serialize for SearchResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.results.len()))?;
        for result in &self.results {
            sequence.serialize_element(&OrderedSearchResult {
                kind: self.kind,
                result,
            })?;
        }
        sequence.end()
    }
}

struct OrderedSearchResult<'a> {
    kind: SearchKind,
    result: &'a SearchResult,
}

impl Serialize for OrderedSearchResult<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let field_order = self.kind.result_field_order();
        let mut map = serializer.serialize_map(Some(self.result.len()))?;
        for field in field_order {
            if let Some(value) = self.result.get(*field) {
                map.serialize_entry(field, value)?;
            }
        }
        for (field, value) in self.result {
            if !field_order.contains(&field.as_str()) {
                map.serialize_entry(field, value)?;
            }
        }
        map.end()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchResultPayload {
    pub results: Vec<SearchResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusResponse {
    pub extension_connected: bool,
    pub browser_instance_id: Option<String>,
    pub extension_version: Option<String>,
    pub active_jobs: usize,
    pub queued_jobs: usize,
    pub max_concurrency: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloPayload {
    pub extension_token: String,
    pub browser_instance_id: String,
    pub browser_name: String,
    pub extension_version: String,
    pub protocol_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeMetadata {
    pub browser_instance_id: String,
    pub browser_name: String,
    pub extension_version: String,
    pub protocol_version: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboundEnvelope {
    pub version: u32,
    #[serde(rename = "type")]
    pub message_type: String,
    #[serde(default)]
    pub request_id: Option<Uuid>,
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_search_kinds_are_supported() {
        assert_eq!(SearchKind::from_str("web").unwrap(), SearchKind::Web);
        assert_eq!(SearchKind::from_str("news").unwrap(), SearchKind::News);
        assert_eq!(SearchKind::from_str("images").unwrap(), SearchKind::Images);
        assert_eq!(SearchKind::from_str("videos").unwrap(), SearchKind::Videos);
        assert_eq!(SearchKind::from_str("forums").unwrap(), SearchKind::Forums);
    }

    #[test]
    fn search_responses_use_endpoint_field_order() {
        fn serialize(kind: SearchKind, fields: &[&str]) -> String {
            let result = fields
                .iter()
                .map(|field| ((*field).to_owned(), (*field).to_owned()))
                .collect();
            serde_json::to_string(&SearchResponse::new(kind, vec![result])).unwrap()
        }

        assert_eq!(
            serialize(SearchKind::Web, &["url", "description", "title"]),
            r#"[{"title":"title","description":"description","url":"url"}]"#
        );
        assert_eq!(
            serialize(
                SearchKind::News,
                &["time", "source", "url", "description", "title"]
            ),
            r#"[{"title":"title","description":"description","url":"url","source":"source","time":"time"}]"#
        );
        assert_eq!(
            serialize(SearchKind::Images, &["url", "imgurl", "title"]),
            r#"[{"title":"title","imgurl":"imgurl","url":"url"}]"#
        );
        assert_eq!(
            serialize(
                SearchKind::Videos,
                &["duration", "url", "description", "title"]
            ),
            r#"[{"title":"title","description":"description","url":"url","duration":"duration"}]"#
        );
        assert_eq!(
            serialize(SearchKind::Forums, &["url", "description", "title"]),
            r#"[{"title":"title","description":"description","url":"url"}]"#
        );
    }

    #[test]
    fn request_is_trimmed_and_validated() {
        let request = SearchRequest {
            query: "  Tokyo  ".into(),
            start: 10,
            limit: 10,
            timeout_ms: Some(30_000),
        }
        .normalized(&Config::default())
        .unwrap();
        assert_eq!(request.query, "Tokyo");
    }

    #[test]
    fn command_is_built_from_search_config() {
        let mut config = Config::default();
        config
            .search
            .common
            .params
            .insert("udm".into(), "common".into());
        let request = SearchRequest {
            query: "Tokyo test".into(),
            start: 10,
            limit: 20,
            timeout_ms: Some(30_000),
        }
        .normalized(&config)
        .unwrap();
        let command = SearchCommand::new(SearchKind::Images, request, &config).unwrap();
        let url = url::Url::parse(&command.url).unwrap();
        let udm_values = url
            .query_pairs()
            .filter(|(name, _)| name == "udm")
            .map(|(_, value)| value.into_owned())
            .collect::<Vec<_>>();
        assert_eq!(udm_values, ["2"]);
        assert!(!url.query_pairs().any(|(name, _)| name == "start"));
        assert_eq!(
            url.query_pairs().find(|(name, _)| name == "num").unwrap().1,
            "100"
        );
        assert_eq!(command.limit, 100);
        assert_eq!(
            command.extraction.root_selectors,
            ["[data-snf][data-snm] [data-lpage]"]
        );
        assert_eq!(
            command.extraction.fields["imgurl"].attribute.as_deref(),
            Some("src")
        );
        assert!(matches!(
            command.extraction.fields["imgurl"].transform,
            FieldTransform::AbsoluteUrl
        ));
        assert_eq!(command.extraction.fields["url"].selectors, ["&"]);
        assert!(!command.extraction.fields.contains_key("image"));

        let request = SearchRequest {
            query: "Tokyo test".into(),
            start: 10,
            limit: 20,
            timeout_ms: Some(30_000),
        }
        .normalized(&config)
        .unwrap();
        let command = SearchCommand::new(SearchKind::Web, request, &config).unwrap();
        let url = url::Url::parse(&command.url).unwrap();
        assert_eq!(command.limit, 20);
        assert_eq!(
            url.query_pairs()
                .find(|(name, _)| name == "start")
                .unwrap()
                .1,
            "10"
        );
        assert_eq!(
            url.query_pairs().find(|(name, _)| name == "num").unwrap().1,
            "20"
        );
    }

    #[test]
    fn omitted_timeout_uses_configured_maximum_below_default() {
        let mut config = Config::default();
        config.executor.max_timeout_ms = 1_000;
        let request: SearchRequest = serde_json::from_value(serde_json::json!({
            "query": "Tokyo"
        }))
        .unwrap();

        let normalized = request.normalized(&config).unwrap();

        assert_eq!(normalized.timeout_ms, 1_000);

        let explicit: SearchRequest = serde_json::from_value(serde_json::json!({
            "query": "Tokyo",
            "timeoutMs": 30_000
        }))
        .unwrap();
        assert!(explicit.normalized(&config).is_err());
    }
}
