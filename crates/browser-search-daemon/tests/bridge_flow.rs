use std::time::Duration;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use browser_search_daemon::{AppState, api, bridge, config::Config};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn http_search_round_trips_through_extension_bridge() {
    let mut config = Config::default();
    config.server.api_token = "api-token".into();
    config.bridge.extension_token = "extension-token".into();
    config.bridge.browser_instance_id = "test-browser".into();
    let state = AppState::new(config);
    state.scheduler.clone().start();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let bridge_state = state.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, bridge::router(bridge_state))
            .await
            .unwrap();
    });

    let (mut socket, _) = connect_async(format!("ws://{address}/bridge"))
        .await
        .unwrap();
    socket
        .send(Message::Text(
            json!({
                "version": 1,
                "type": "hello",
                "payload": {
                    "extensionToken": "extension-token",
                    "browserInstanceId": "test-browser",
                    "browserName": "Chrome",
                    "extensionVersion": "0.1.0",
                    "protocolVersion": 1
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let welcome = socket.next().await.unwrap().unwrap();
    assert!(welcome.into_text().unwrap().contains("welcome"));

    let api_state = state.clone();
    let request = tokio::spawn(async move {
        api::router(api_state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/search/news")
                    .header("authorization", "Bearer api-token")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"query":"Tokyo","limit":5}"#))
                    .unwrap(),
            )
            .await
            .unwrap()
    });

    let search_message = loop {
        let message = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let parsed: Value = serde_json::from_str(&message).unwrap();
        if parsed["type"] == "search" {
            break parsed;
        }
    };
    assert_eq!(search_message["payload"]["kind"], "news");
    assert_eq!(search_message["payload"]["query"], "Tokyo");
    assert_eq!(
        search_message["payload"]["extraction"]["rootSelectors"][0],
        "[data-news-cluster-id]"
    );
    assert_eq!(
        search_message["payload"]["extraction"]["fields"]["title"]["selectors"][0],
        "a > div > div:nth-of-type(2) > div:nth-of-type(2)"
    );
    let request_id = search_message["requestId"].as_str().unwrap();

    socket
        .send(Message::Text(
            json!({
                "version": 1,
                "type": "search_result",
                "requestId": request_id,
                "payload": {
                    "results": [{
                        "url": "https://example.com/tokyo",
                        "title": "Tokyo",
                        "description": "News from Tokyo",
                        "source": "Example News",
                        "time": "2 hours ago"
                    }]
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

    let response = request.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        std::str::from_utf8(&body).unwrap(),
        r#"[{"title":"Tokyo","description":"News from Tokyo","url":"https://example.com/tokyo","source":"Example News","time":"2 hours ago"}]"#
    );
    let results: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(results[0]["url"], "https://example.com/tokyo");
    assert_eq!(results[0]["title"], "Tokyo");
    assert_eq!(results[0]["source"], "Example News");
    assert_eq!(results[0]["time"], "2 hours ago");

    server.abort();
}

#[tokio::test]
async fn cleanup_confirmation_and_reconnect_owner_are_enforced() {
    let mut config = Config::default();
    config.server.api_token = "api-token".into();
    config.bridge.extension_token = "extension-token".into();
    config.executor.max_concurrency = 1;
    config.executor.min_operation_interval = 80;
    let state = AppState::new(config);
    state.scheduler.clone().start();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let bridge_state = state.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, bridge::router(bridge_state))
            .await
            .unwrap();
    });

    let (mut socket, _) = connect_async(format!("ws://{address}/bridge"))
        .await
        .unwrap();
    socket
        .send(Message::Text(
            json!({
                "version": 1,
                "type": "hello",
                "payload": {
                    "extensionToken": "extension-token",
                    "browserInstanceId": "test-browser",
                    "browserName": "Chrome",
                    "extensionVersion": "0.1.0",
                    "protocolVersion": 1
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let welcome: Value = serde_json::from_str(
        socket
            .next()
            .await
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap()
            .as_ref(),
    )
    .unwrap();
    assert_eq!(welcome["type"], "welcome");
    assert_eq!(welcome["payload"]["minOperationIntervalMs"], 80);

    let spawn_request = |state: AppState, query: &'static str| {
        tokio::spawn(async move {
            api::router(state)
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/search/web")
                        .header("authorization", "Bearer api-token")
                        .header("content-type", "application/json")
                        .body(Body::from(format!(r#"{{"query":"{query}"}}"#)))
                        .unwrap(),
                )
                .await
                .unwrap()
        })
    };

    let cancelled_request = spawn_request(state.clone(), "cancelled");
    let cancelled_search = loop {
        let message = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let parsed: Value = serde_json::from_str(&message).unwrap();
        if parsed["type"] == "search" {
            break parsed;
        }
    };
    let cancelled_id = cancelled_search["requestId"].as_str().unwrap();
    state
        .scheduler
        .cancel(Uuid::parse_str(cancelled_id).unwrap(), "test cancellation")
        .await;
    assert_eq!(
        cancelled_request.await.unwrap().status(),
        StatusCode::CONFLICT
    );
    assert_eq!(state.scheduler.active_jobs(), 1);
    loop {
        let message = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let parsed: Value = serde_json::from_str(&message).unwrap();
        if parsed["type"] == "cancel" {
            assert_eq!(parsed["requestId"], cancelled_id);
            break;
        }
    }

    let after_cancel_request = spawn_request(state.clone(), "after-cancel");
    assert!(
        tokio::time::timeout(Duration::from_millis(30), socket.next())
            .await
            .is_err()
    );
    socket
        .send(Message::Text(
            json!({
                "version": 1,
                "type": "cleanup_complete",
                "requestId": cancelled_id,
                "payload": {}
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

    let after_cancel_search = loop {
        let message = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let parsed: Value = serde_json::from_str(&message).unwrap();
        if parsed["type"] == "search" {
            break parsed;
        }
    };
    let after_cancel_id = after_cancel_search["requestId"].as_str().unwrap();
    socket
        .send(Message::Text(
            json!({
                "version": 1,
                "type": "search_result",
                "requestId": after_cancel_id,
                "payload": { "results": [{ "title": "after", "url": "https://after.example" }] }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    assert_eq!(after_cancel_request.await.unwrap().status(), StatusCode::OK);

    let disconnected_request = spawn_request(state.clone(), "disconnected");
    loop {
        let message = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let parsed: Value = serde_json::from_str(&message).unwrap();
        if parsed["type"] == "search" {
            break;
        }
    }
    socket.send(Message::Close(None)).await.unwrap();
    assert_eq!(
        disconnected_request.await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(state.scheduler.active_jobs(), 1);
    tokio::time::sleep(Duration::from_millis(10)).await;

    let (mut other_socket, _) = connect_async(format!("ws://{address}/bridge"))
        .await
        .unwrap();
    other_socket
        .send(Message::Text(
            json!({
                "version": 1,
                "type": "hello",
                "payload": {
                    "extensionToken": "extension-token",
                    "browserInstanceId": "other-browser",
                    "browserName": "Chrome",
                    "extensionVersion": "0.1.0",
                    "protocolVersion": 1
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let conflict: Value = serde_json::from_str(
        other_socket
            .next()
            .await
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap()
            .as_ref(),
    )
    .unwrap();
    assert_eq!(conflict["payload"]["code"], "instance_conflict");
    assert!(
        conflict["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("完成任务清理")
    );

    let (mut reconnected_socket, _) = connect_async(format!("ws://{address}/bridge"))
        .await
        .unwrap();
    reconnected_socket
        .send(Message::Text(
            json!({
                "version": 1,
                "type": "hello",
                "payload": {
                    "extensionToken": "extension-token",
                    "browserInstanceId": "test-browser",
                    "browserName": "Chrome",
                    "extensionVersion": "0.1.0",
                    "protocolVersion": 1
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let welcome = reconnected_socket.next().await.unwrap().unwrap();
    assert!(welcome.into_text().unwrap().contains("welcome"));
    assert_eq!(state.scheduler.active_jobs(), 0);

    server.abort();
}
