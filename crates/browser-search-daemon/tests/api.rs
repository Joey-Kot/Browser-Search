use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use browser_search_daemon::{AppState, api, config::Config};
use tower::ServiceExt;

fn test_state() -> AppState {
    let mut config = Config::default();
    config.server.api_token = "api-token".into();
    config.bridge.extension_token = "extension-token".into();
    let state = AppState::new(config);
    state.scheduler.clone().start();
    state
}

#[tokio::test]
async fn protected_routes_require_bearer_token() {
    let response = api::router(test_state())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/search/web")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"Tokyo"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn search_reports_missing_extension() {
    let response = api::router(test_state())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/search/news")
                .header("authorization", "Bearer api-token")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"Tokyo"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}
