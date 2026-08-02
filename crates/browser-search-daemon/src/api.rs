use std::{str::FromStr, time::Duration};

use axum::{
    Json, Router,
    extract::{Path, Request, State, rejection::JsonRejection},
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, StatusCode,
        header::{AUTHORIZATION, CONTENT_TYPE},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use tower_http::cors::{Any, CorsLayer};

use crate::{
    AppState,
    error::{ApiError, ErrorCode, ErrorDetail, ErrorResponse},
    model::{
        HealthResponse, SearchCommand, SearchKind, SearchRequest, SearchResponse, StatusResponse,
    },
};

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/v1/search/{kind}", post(search))
        .route("/v1/status", get(status))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer,
        ));

    let router = Router::new()
        .route("/health", get(health))
        .merge(protected)
        .with_state(state.clone());

    if state.config.server.allow_cors {
        router.layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([Method::GET, Method::POST])
                .allow_headers([AUTHORIZATION, CONTENT_TYPE]),
        )
    } else {
        router
    }
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn status(State(state): State<AppState>) -> Json<StatusResponse> {
    let metadata = state.bridge.metadata().await;
    Json(StatusResponse {
        extension_connected: metadata.is_some(),
        browser_instance_id: metadata
            .as_ref()
            .map(|metadata| metadata.browser_instance_id.clone()),
        extension_version: metadata.map(|metadata| metadata.extension_version),
        active_jobs: state.scheduler.active_jobs(),
        queued_jobs: state.scheduler.queued_jobs(),
        max_concurrency: state.scheduler.max_concurrency(),
    })
}

async fn search(
    State(state): State<AppState>,
    Path(kind): Path<String>,
    payload: Result<Json<SearchRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let kind = SearchKind::from_str(&kind).map_err(ApiError::from_detail)?;
    let Json(request) = payload.map_err(|error| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            ErrorDetail::new(
                ErrorCode::InvalidRequest,
                format!("请求 JSON 无效: {}", error.body_text()),
                false,
            ),
        )
    })?;
    let request = request
        .normalized(&state.config)
        .map_err(ApiError::from_detail)?;
    let wait_ms = request.timeout_ms.saturating_add(1_000);
    let command =
        SearchCommand::new(kind, request, &state.config).map_err(ApiError::from_detail)?;
    let (request_id, receiver) = state
        .scheduler
        .enqueue(command)
        .await
        .map_err(ApiError::from_detail)?;

    let outcome = tokio::time::timeout(Duration::from_millis(wait_ms), receiver).await;
    let results = match outcome {
        Ok(Ok(Ok(results))) => results,
        Ok(Ok(Err(error))) => return Err(ApiError::from_detail(error)),
        Ok(Err(_)) => {
            return Err(ApiError::from_detail(ErrorDetail::new(
                ErrorCode::InternalError,
                "搜索任务响应通道意外关闭",
                true,
            )));
        }
        Err(_) => {
            state
                .scheduler
                .cancel(request_id, "HTTP 请求等待超时")
                .await;
            return Err(ApiError::from_detail(ErrorDetail::timeout(
                "HTTP 请求等待搜索结果超时",
            )));
        }
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("x-request-id"),
        HeaderValue::from_str(&request_id.to_string()).expect("UUID is a valid header value"),
    );
    Ok((headers, Json(SearchResponse::new(kind, results))).into_response())
}

async fn require_bearer(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let expected = format!("Bearer {}", state.config.server.api_token);
    let authorized = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| constant_time_eq(value.as_bytes(), expected.as_bytes()))
        .unwrap_or(false);

    if !authorized {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: ErrorDetail::new(
                    ErrorCode::Unauthorized,
                    "缺少或提供了无效的 Bearer token",
                    false,
                ),
            }),
        )
            .into_response();
    }
    next.run(request).await
}

pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}
