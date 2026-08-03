mod args;
mod client;
mod error;
mod model;

use std::{
    collections::HashSet,
    env,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use futures_util::{StreamExt, stream};
use url::Url;

pub use args::Cli;
pub use error::CliError;
pub use model::SearchResult;

use client::SearchClient;
use model::{SearchKind, SearchRequest};

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:17330";
const PAGE_SIZE: u32 = 10;

#[derive(Debug, Clone)]
pub struct Environment {
    pub base_url: Option<String>,
    pub api_token: Option<String>,
}

impl Environment {
    pub fn from_process() -> Self {
        Self {
            base_url: env::var("SEARCH_BASE_URL").ok(),
            api_token: env::var("SEARCH_API_KEY").ok(),
        }
    }

    fn resolve(self) -> Result<ResolvedEnvironment, CliError> {
        let api_token = self
            .api_token
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| CliError::usage("SEARCH_API_KEY is required"))?;

        let raw_base_url = self
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_BASE_URL);
        let mut base_url = Url::parse(raw_base_url)
            .map_err(|error| CliError::usage(format!("SEARCH_BASE_URL is invalid: {error}")))?;
        if base_url.query().is_some() || base_url.fragment().is_some() {
            return Err(CliError::usage(
                "SEARCH_BASE_URL must not contain a query string or fragment",
            ));
        }
        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path());
            base_url.set_path(&path);
        }

        Ok(ResolvedEnvironment {
            base_url,
            api_token,
        })
    }
}

#[derive(Debug)]
struct ResolvedEnvironment {
    base_url: Url,
    api_token: String,
}

pub async fn execute(cli: Cli) -> Result<Vec<SearchResult>, CliError> {
    execute_with_environment(cli, Environment::from_process()).await
}

pub async fn execute_with_environment(
    cli: Cli,
    environment: Environment,
) -> Result<Vec<SearchResult>, CliError> {
    let resolved = cli.resolve()?;
    let environment = environment.resolve()?;
    let client = SearchClient::new(
        environment.base_url,
        environment.api_token,
        Duration::from_secs(resolved.timeout_seconds),
    )?;
    let status = client.status().await?;
    if !status.extension_connected {
        return Err(CliError::runtime("Chrome extension is not connected"));
    }

    let requests = page_requests(
        resolved.kind,
        &resolved.query,
        resolved.search_num,
        resolved.search_timeout_ms,
    );
    let concurrency = status.max_concurrency.max(1).min(requests.len());
    let failed = AtomicBool::new(false);
    let mut pages = stream::iter(requests.into_iter().enumerate().map(|(index, request)| {
        let client = &client;
        let failed = &failed;
        async move {
            if failed.load(Ordering::Acquire) {
                return None;
            }
            let outcome = client.search(resolved.kind, &request).await;
            if outcome.is_err() {
                failed.store(true, Ordering::Release);
            }
            Some((index, outcome))
        }
    }))
    .buffer_unordered(concurrency)
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    pages.sort_by_key(|(index, _)| *index);

    let mut merged = Vec::new();
    for (_, page) in pages {
        merged.extend(page?);
    }
    stable_dedupe(&mut merged);
    merged.truncate(resolved.search_num as usize);
    Ok(merged)
}

fn page_requests(
    kind: SearchKind,
    query: &str,
    search_num: u32,
    timeout_ms: Option<u64>,
) -> Vec<SearchRequest> {
    if kind.is_images() {
        return vec![SearchRequest {
            query: query.to_owned(),
            start: None,
            limit: None,
            timeout_ms,
        }];
    }

    let page_count = search_num.div_ceil(PAGE_SIZE);
    (0..page_count)
        .map(|page| SearchRequest {
            query: query.to_owned(),
            start: Some(page * PAGE_SIZE),
            limit: Some(PAGE_SIZE),
            timeout_ms,
        })
        .collect()
}

fn stable_dedupe(results: &mut Vec<SearchResult>) {
    let mut seen = HashSet::new();
    results.retain(|result| {
        let key = result
            .get("url")
            .filter(|url| !url.is_empty())
            .map(|url| format!("url\0{url}"))
            .unwrap_or_else(|| {
                serde_json::to_string(result)
                    .expect("a search result containing only strings is valid JSON")
            });
        seen.insert(key)
    });
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use axum::{
        Json, Router,
        extract::{Path, State},
        http::{HeaderMap, StatusCode, header::AUTHORIZATION},
        response::{IntoResponse, Response},
        routing::{get, post},
    };
    use clap::Parser;
    use indexmap::IndexMap;
    use serde_json::{Value, json};
    use tokio::net::TcpListener;

    use super::{Cli, Environment, execute_with_environment, stable_dedupe};

    #[derive(Clone)]
    struct MockState {
        requests: Arc<Mutex<Vec<(String, Value)>>>,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
        max_concurrency: usize,
        fail_start: Option<u32>,
    }

    async fn status(State(state): State<MockState>, headers: HeaderMap) -> Json<Value> {
        assert_eq!(
            headers.get(AUTHORIZATION).unwrap(),
            "Bearer integration-token"
        );
        Json(json!({
            "extensionConnected": true,
            "activeJobs": 0,
            "queuedJobs": 0,
            "maxConcurrency": state.max_concurrency
        }))
    }

    async fn search(
        State(state): State<MockState>,
        Path(kind): Path<String>,
        headers: HeaderMap,
        Json(request): Json<Value>,
    ) -> Response {
        assert_eq!(
            headers.get(AUTHORIZATION).unwrap(),
            "Bearer integration-token"
        );
        state
            .requests
            .lock()
            .unwrap()
            .push((kind.clone(), request.clone()));

        let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
        state.max_active.fetch_max(active, Ordering::SeqCst);
        let start = request["start"].as_u64().unwrap_or(0) as u32;
        if state.fail_start == Some(start) {
            state.active.fetch_sub(1, Ordering::SeqCst);
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "error": {
                        "code": "extraction_failed",
                        "message": "configured page failure",
                        "retryable": false
                    }
                })),
            )
                .into_response();
        }
        tokio::time::sleep(Duration::from_millis(u64::from(35 - start.min(30)))).await;
        state.active.fetch_sub(1, Ordering::SeqCst);

        let count = if kind == "images" { 12 } else { 10 };
        let results = (0..count)
            .map(|offset| {
                let position = start + offset;
                let mut result = IndexMap::new();
                result.insert("title".to_owned(), format!("result-{position}"));
                result.insert("description".to_owned(), format!("description-{position}"));
                result.insert("url".to_owned(), format!("https://example.com/{position}"));
                result
            })
            .collect::<Vec<IndexMap<String, String>>>();
        Json(results).into_response()
    }

    async fn mock_server(
        max_concurrency: usize,
    ) -> (MockState, String, tokio::task::JoinHandle<()>) {
        mock_server_with_failure(max_concurrency, None).await
    }

    async fn mock_server_with_failure(
        max_concurrency: usize,
        fail_start: Option<u32>,
    ) -> (MockState, String, tokio::task::JoinHandle<()>) {
        let state = MockState {
            requests: Arc::new(Mutex::new(Vec::new())),
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
            max_concurrency,
            fail_start,
        };
        let app = Router::new()
            .route("/v1/status", get(status))
            .route("/v1/search/{kind}", post(search))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (state, format!("http://{address}"), server)
    }

    fn environment(base_url: String) -> Environment {
        Environment {
            base_url: Some(base_url),
            api_token: Some("integration-token".to_owned()),
        }
    }

    #[tokio::test]
    async fn paginates_concurrently_merges_in_page_order_and_preserves_field_order() {
        let (state, base_url, server) = mock_server(3).await;
        let cli = Cli::try_parse_from([
            "search",
            "web",
            "--query",
            "Tokyo",
            "--search-num",
            "100",
            "--search-timeout",
            "30",
        ])
        .unwrap();

        let results = execute_with_environment(cli, environment(base_url))
            .await
            .unwrap();
        assert_eq!(results.len(), 100);
        assert_eq!(
            results[0].keys().map(String::as_str).collect::<Vec<_>>(),
            ["title", "description", "url"]
        );
        assert_eq!(results[0]["title"], "result-0");
        assert_eq!(results[10]["title"], "result-10");
        assert_eq!(results[99]["title"], "result-99");
        assert_eq!(state.max_active.load(Ordering::SeqCst), 3);

        let requests = state.requests.lock().unwrap();
        assert_eq!(requests.len(), 10);
        let mut starts = requests
            .iter()
            .map(|(kind, request)| {
                assert_eq!(kind, "web");
                assert_eq!(request["limit"], 10);
                assert_eq!(request["timeoutMs"], 30_000);
                request["start"].as_u64().unwrap()
            })
            .collect::<Vec<_>>();
        starts.sort_unstable();
        assert_eq!(starts, [0, 10, 20, 30, 40, 50, 60, 70, 80, 90]);
        drop(requests);
        server.abort();
    }

    #[tokio::test]
    async fn arbitrary_result_counts_round_up_pages_then_truncate() {
        let (state, base_url, server) = mock_server(2).await;
        let cli = Cli::try_parse_from(["search", "web", "--query", "Tokyo", "--search-num", "25"])
            .unwrap();

        let results = execute_with_environment(cli, environment(base_url))
            .await
            .unwrap();
        assert_eq!(results.len(), 25);

        let requests = state.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        let mut starts = requests
            .iter()
            .map(|(_, request)| request["start"].as_u64().unwrap())
            .collect::<Vec<_>>();
        starts.sort_unstable();
        assert_eq!(starts, [0, 10, 20]);
        drop(requests);
        server.abort();
    }

    #[tokio::test]
    async fn page_failure_stops_scheduling_remaining_requests() {
        let (state, base_url, server) = mock_server_with_failure(3, Some(0)).await;
        let cli = Cli::try_parse_from(["search", "web", "--query", "Tokyo", "--search-num", "100"])
            .unwrap();

        let error = execute_with_environment(cli, environment(base_url))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("extraction_failed"));
        let requests = state.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        let mut starts = requests
            .iter()
            .map(|(_, request)| request["start"].as_u64().unwrap())
            .collect::<Vec<_>>();
        starts.sort_unstable();
        assert_eq!(starts, [0, 10, 20]);
        drop(requests);
        server.abort();
    }

    #[tokio::test]
    async fn images_use_one_full_page_request_then_apply_the_requested_count() {
        let (state, base_url, server) = mock_server(4).await;
        let cli =
            Cli::try_parse_from(["search", "images", "--query", "Tokyo", "--search-num", "5"])
                .unwrap();

        let results = execute_with_environment(cli, environment(base_url))
            .await
            .unwrap();
        assert_eq!(results.len(), 5);
        let requests = state.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "images");
        assert!(requests[0].1.get("start").is_none());
        assert!(requests[0].1.get("limit").is_none());
        drop(requests);
        server.abort();
    }

    #[test]
    fn merged_pages_keep_the_first_result_for_each_url() {
        let mut first = IndexMap::new();
        first.insert("title".to_owned(), "first".to_owned());
        first.insert("url".to_owned(), "https://example.com/shared".to_owned());
        let mut duplicate = IndexMap::new();
        duplicate.insert("title".to_owned(), "duplicate".to_owned());
        duplicate.insert("url".to_owned(), "https://example.com/shared".to_owned());
        let mut unique = IndexMap::new();
        unique.insert("title".to_owned(), "unique".to_owned());
        unique.insert("url".to_owned(), "https://example.com/unique".to_owned());

        let mut results = vec![first, duplicate, unique];
        stable_dedupe(&mut results);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["title"], "first");
        assert_eq!(results[1]["title"], "unique");
    }

    #[test]
    fn environment_requires_a_token_and_normalizes_the_base_url() {
        let missing = Environment {
            base_url: None,
            api_token: None,
        }
        .resolve()
        .unwrap_err();
        assert_eq!(missing.exit_code(), 2);

        let resolved = Environment {
            base_url: Some("http://127.0.0.1:17330/base".to_owned()),
            api_token: Some(" token ".to_owned()),
        }
        .resolve()
        .unwrap();
        assert_eq!(resolved.base_url.as_str(), "http://127.0.0.1:17330/base/");
        assert_eq!(resolved.api_token, "token");
    }

    #[test]
    fn environment_accepts_https_base_urls() {
        let resolved = Environment {
            base_url: Some("https://search.example.test/api".to_owned()),
            api_token: Some("token".to_owned()),
        }
        .resolve()
        .unwrap();

        assert_eq!(
            resolved.base_url.as_str(),
            "https://search.example.test/api/"
        );
    }
}
