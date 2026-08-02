use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{RwLock, mpsc};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    AppState,
    api::constant_time_eq,
    config::Config,
    error::{ErrorCode, ErrorDetail},
    model::{BridgeMetadata, HelloPayload, InboundEnvelope, SearchCommand, SearchResultPayload},
};

pub const PROTOCOL_VERSION: u32 = 1;

struct BridgeConnection {
    session_id: Uuid,
    metadata: BridgeMetadata,
    sender: mpsc::Sender<String>,
    closing: bool,
}

pub struct BridgeHub {
    config: Arc<Config>,
    connection: RwLock<Option<BridgeConnection>>,
}

impl BridgeHub {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            connection: RwLock::new(None),
        }
    }

    async fn register(
        &self,
        hello: HelloPayload,
        sender: mpsc::Sender<String>,
        required_cleanup_instance: Option<&str>,
    ) -> Result<Uuid, ErrorDetail> {
        if !constant_time_eq(
            hello.extension_token.as_bytes(),
            self.config.bridge.extension_token.as_bytes(),
        ) {
            return Err(ErrorDetail::new(
                ErrorCode::Unauthorized,
                "扩展 token 无效",
                false,
            ));
        }
        if hello.protocol_version != PROTOCOL_VERSION {
            return Err(ErrorDetail::new(
                ErrorCode::ProtocolError,
                "扩展协议版本不受支持",
                false,
            ));
        }
        if !self.config.bridge.browser_instance_id.is_empty()
            && self.config.bridge.browser_instance_id != hello.browser_instance_id
        {
            return Err(ErrorDetail::new(
                ErrorCode::InstanceConflict,
                "浏览器实例与配置不匹配",
                false,
            ));
        }
        if required_cleanup_instance
            .is_some_and(|instance_id| instance_id != hello.browser_instance_id)
        {
            return Err(ErrorDetail::new(
                ErrorCode::InstanceConflict,
                "原 Chrome 扩展实例仍需完成任务清理",
                true,
            ));
        }

        let mut connection = self.connection.write().await;
        if connection.is_some() {
            return Err(ErrorDetail::new(
                ErrorCode::InstanceConflict,
                "已有 Chrome 扩展实例连接",
                true,
            ));
        }

        let session_id = Uuid::new_v4();
        *connection = Some(BridgeConnection {
            session_id,
            metadata: BridgeMetadata {
                browser_instance_id: hello.browser_instance_id,
                browser_name: hello.browser_name,
                extension_version: hello.extension_version,
                protocol_version: hello.protocol_version,
            },
            sender,
            closing: false,
        });
        Ok(session_id)
    }

    async fn begin_disconnect(&self, session_id: Uuid) -> bool {
        let mut connection = self.connection.write().await;
        let Some(active) = connection.as_mut() else {
            return false;
        };
        if active.session_id != session_id || active.closing {
            return false;
        }
        active.closing = true;
        true
    }

    async fn disconnect(&self, session_id: Uuid) -> bool {
        let mut connection = self.connection.write().await;
        let matches = connection
            .as_ref()
            .map(|active| active.session_id == session_id)
            .unwrap_or(false);
        if matches {
            connection.take();
        }
        matches
    }

    pub async fn is_connected(&self) -> bool {
        self.connection
            .read()
            .await
            .as_ref()
            .is_some_and(|connection| !connection.closing)
    }

    pub async fn metadata(&self) -> Option<BridgeMetadata> {
        self.connection
            .read()
            .await
            .as_ref()
            .filter(|connection| !connection.closing)
            .map(|connection| connection.metadata.clone())
    }

    pub async fn send_search(
        &self,
        request_id: Uuid,
        command: &SearchCommand,
    ) -> Result<(), ErrorDetail> {
        self.send_json(json!({
            "version": PROTOCOL_VERSION,
            "type": "search",
            "requestId": request_id,
            "payload": command,
        }))
        .await
    }

    pub async fn send_cancel(&self, request_id: Uuid, reason: &str) -> Result<(), ErrorDetail> {
        self.send_json(json!({
            "version": PROTOCOL_VERSION,
            "type": "cancel",
            "requestId": request_id,
            "payload": { "reason": reason },
        }))
        .await
    }

    async fn send_json(&self, value: Value) -> Result<(), ErrorDetail> {
        let sender = self
            .connection
            .read()
            .await
            .as_ref()
            .filter(|connection| !connection.closing)
            .map(|connection| connection.sender.clone())
            .ok_or_else(|| ErrorDetail::browser_unavailable("Chrome 扩展未连接"))?;
        sender.try_send(value.to_string()).map_err(|error| {
            ErrorDetail::browser_unavailable(format!("扩展连接写入队列不可用: {error}"))
        })
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/bridge", get(upgrade))
        .with_state(state)
}

async fn upgrade(State(state): State<AppState>, websocket: WebSocketUpgrade) -> impl IntoResponse {
    websocket.on_upgrade(move |socket| run_socket(state, socket))
}

async fn run_socket(state: AppState, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let first_message = tokio::time::timeout(Duration::from_secs(10), stream.next()).await;
    let hello = match first_message {
        Ok(Some(Ok(Message::Text(text)))) => match parse_hello(text.as_str()) {
            Ok(hello) => hello,
            Err(error) => {
                let _ = sink
                    .send(Message::Text(error_message(None, &error).into()))
                    .await;
                return;
            }
        },
        _ => {
            let error = ErrorDetail::new(ErrorCode::ProtocolError, "连接后必须先发送 hello", false);
            let _ = sink
                .send(Message::Text(error_message(None, &error).into()))
                .await;
            return;
        }
    };

    let (sender, mut receiver) = mpsc::channel::<String>(128);
    let required_cleanup_instance = state.scheduler.required_cleanup_instance().await;
    let session_id = match state
        .bridge
        .register(
            hello.clone(),
            sender.clone(),
            required_cleanup_instance.as_deref(),
        )
        .await
    {
        Ok(session_id) => session_id,
        Err(error) => {
            let _ = sink
                .send(Message::Text(error_message(None, &error).into()))
                .await;
            return;
        }
    };
    state
        .scheduler
        .handle_reconnect(&hello.browser_instance_id)
        .await;

    info!(
        browser_instance_id = %hello.browser_instance_id,
        extension_version = %hello.extension_version,
        "extension connected"
    );

    let welcome = json!({
        "version": PROTOCOL_VERSION,
        "type": "welcome",
        "payload": {
            "protocolVersion": PROTOCOL_VERSION,
            "sessionId": session_id,
            "pingIntervalSeconds": state.config.bridge.ping_interval_seconds,
            "minOperationIntervalMs": state.config.executor.min_operation_interval,
        }
    })
    .to_string();
    if sink.send(Message::Text(welcome.into())).await.is_err() {
        disconnect_session(&state, session_id, &hello.browser_instance_id).await;
        return;
    }

    let ping_seconds = state.config.bridge.ping_interval_seconds;
    let mut writer = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(ping_seconds));
        interval.tick().await;
        loop {
            tokio::select! {
                message = receiver.recv() => {
                    match message {
                        Some(message) => {
                            if sink.send(Message::Text(message.into())).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                _ = interval.tick() => {
                    let ping = json!({
                        "version": PROTOCOL_VERSION,
                        "type": "ping",
                        "payload": { "nonce": Uuid::new_v4().to_string() }
                    }).to_string();
                    if sink.send(Message::Text(ping.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    let writer_finished = loop {
        tokio::select! {
            writer_result = &mut writer => {
                if let Err(error) = writer_result {
                    warn!(%error, "bridge writer task failed");
                }
                break true;
            }
            message = stream.next() => {
                let Some(message) = message else {
                    break false;
                };
                match message {
                    Ok(Message::Text(text)) => {
                        if let Err(error) = handle_message(&state, text.as_str()).await {
                            warn!(message = %error.message, "bridge protocol error");
                            let envelope = serde_json::from_str::<InboundEnvelope>(text.as_str()).ok();
                            let request_id = envelope.as_ref().and_then(|envelope| envelope.request_id);
                            if let Some(id) = request_id {
                                if envelope.as_ref().is_some_and(|envelope| {
                                    matches!(envelope.message_type.as_str(), "search_result" | "error")
                                }) {
                                    state.scheduler.extension_error(id, error.clone()).await;
                                } else {
                                    state.scheduler.protocol_error(id, error.clone()).await;
                                }
                            }
                            let _ = sender.try_send(error_message(request_id, &error));
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break false,
                    Ok(Message::Ping(data)) => {
                        let _ = sender.try_send(
                            json!({
                                "version": PROTOCOL_VERSION,
                                "type": "pong",
                                "payload": { "data": data.to_vec() }
                            })
                            .to_string(),
                        );
                    }
                    Ok(Message::Pong(_)) | Ok(Message::Binary(_)) => {}
                }
            }
        }
    };

    if !writer_finished {
        writer.abort();
        let _ = writer.await;
    }
    disconnect_session(&state, session_id, &hello.browser_instance_id).await;
    info!(session_id = %session_id, "extension disconnected");
}

async fn disconnect_session(state: &AppState, session_id: Uuid, browser_instance_id: &str) {
    if !state.bridge.begin_disconnect(session_id).await {
        return;
    }
    state.scheduler.handle_disconnect(browser_instance_id).await;
    state.bridge.disconnect(session_id).await;
}

fn parse_hello(text: &str) -> Result<HelloPayload, ErrorDetail> {
    let envelope: InboundEnvelope = serde_json::from_str(text).map_err(|error| {
        ErrorDetail::new(
            ErrorCode::ProtocolError,
            format!("hello JSON 无效: {error}"),
            false,
        )
    })?;
    if envelope.version != PROTOCOL_VERSION || envelope.message_type != "hello" {
        return Err(ErrorDetail::new(
            ErrorCode::ProtocolError,
            format!("第一条消息必须是协议版本 {PROTOCOL_VERSION} 的 hello"),
            false,
        ));
    }
    serde_json::from_value(envelope.payload).map_err(|error| {
        ErrorDetail::new(
            ErrorCode::ProtocolError,
            format!("hello payload 无效: {error}"),
            false,
        )
    })
}

async fn handle_message(state: &AppState, text: &str) -> Result<(), ErrorDetail> {
    let envelope: InboundEnvelope = serde_json::from_str(text).map_err(|error| {
        ErrorDetail::new(
            ErrorCode::ProtocolError,
            format!("消息 JSON 无效: {error}"),
            false,
        )
    })?;
    if envelope.version != PROTOCOL_VERSION {
        return Err(ErrorDetail::new(
            ErrorCode::ProtocolError,
            "消息协议版本不受支持",
            false,
        ));
    }

    match envelope.message_type.as_str() {
        "search_result" => {
            let id = envelope.request_id.ok_or_else(|| {
                ErrorDetail::new(ErrorCode::ProtocolError, "缺少 requestId", false)
            })?;
            let payload: SearchResultPayload =
                serde_json::from_value(envelope.payload).map_err(|error| {
                    ErrorDetail::new(
                        ErrorCode::ProtocolError,
                        format!("search_result payload 无效: {error}"),
                        false,
                    )
                })?;
            state.scheduler.complete(id, payload.results).await;
        }
        "error" => {
            let id = envelope.request_id.ok_or_else(|| {
                ErrorDetail::new(ErrorCode::ProtocolError, "error 缺少 requestId", false)
            })?;
            let error: ErrorDetail = serde_json::from_value(envelope.payload).map_err(|error| {
                ErrorDetail::new(
                    ErrorCode::ProtocolError,
                    format!("error payload 无效: {error}"),
                    false,
                )
            })?;
            state.scheduler.extension_error(id, error).await;
        }
        "cleanup_complete" => {
            let id = envelope.request_id.ok_or_else(|| {
                ErrorDetail::new(
                    ErrorCode::ProtocolError,
                    "cleanup_complete 缺少 requestId",
                    false,
                )
            })?;
            state.scheduler.cleanup_complete(id).await;
        }
        "accepted" | "progress" | "pong" => {}
        "ping" => {
            let nonce = envelope
                .payload
                .get("nonce")
                .cloned()
                .unwrap_or(Value::Null);
            state
                .bridge
                .send_json(json!({
                    "version": PROTOCOL_VERSION,
                    "type": "pong",
                    "payload": { "nonce": nonce },
                }))
                .await?;
        }
        other => {
            return Err(ErrorDetail::new(
                ErrorCode::ProtocolError,
                format!("不支持的消息类型: {other}"),
                false,
            ));
        }
    }
    Ok(())
}

fn error_message(request_id: Option<Uuid>, error: &ErrorDetail) -> String {
    json!({
        "version": PROTOCOL_VERSION,
        "type": "error",
        "requestId": request_id,
        "payload": error,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(browser_instance_id: &str) -> HelloPayload {
        HelloPayload {
            extension_token: "extension-token".into(),
            browser_instance_id: browser_instance_id.into(),
            browser_name: "Chrome".into(),
            extension_version: "0.1.0".into(),
            protocol_version: PROTOCOL_VERSION,
        }
    }

    #[tokio::test]
    async fn configured_browser_instance_is_enforced() {
        let mut config = Config::default();
        config.bridge.extension_token = "extension-token".into();
        config.bridge.browser_instance_id = "allowed-browser".into();
        let hub = BridgeHub::new(Arc::new(config));
        let (sender, _receiver) = mpsc::channel(1);

        let error = hub
            .register(hello("other-browser"), sender, None)
            .await
            .unwrap_err();

        assert_eq!(error.code, ErrorCode::InstanceConflict);
        assert!(!error.retryable);
        assert!(!hub.is_connected().await);
    }

    #[tokio::test]
    async fn empty_browser_instance_accepts_extension() {
        let mut config = Config::default();
        config.bridge.extension_token = "extension-token".into();
        let hub = BridgeHub::new(Arc::new(config));
        let (sender, _receiver) = mpsc::channel(1);

        hub.register(hello("any-browser"), sender, None)
            .await
            .unwrap();

        assert_eq!(
            hub.metadata().await.unwrap().browser_instance_id,
            "any-browser"
        );
    }

    #[tokio::test]
    async fn pending_cleanup_requires_the_same_browser_instance() {
        let mut config = Config::default();
        config.bridge.extension_token = "extension-token".into();
        let hub = BridgeHub::new(Arc::new(config));
        let (sender, _receiver) = mpsc::channel(1);

        let error = hub
            .register(hello("other-browser"), sender, Some("cleanup-owner"))
            .await
            .unwrap_err();

        assert_eq!(error.code, ErrorCode::InstanceConflict);
        assert!(error.retryable);
        assert!(!hub.is_connected().await);
    }

    #[tokio::test]
    async fn full_outgoing_queue_is_rejected_without_waiting() {
        let mut config = Config::default();
        config.bridge.extension_token = "extension-token".into();
        let hub = BridgeHub::new(Arc::new(config));
        let (sender, _receiver) = mpsc::channel(1);

        hub.register(hello("any-browser"), sender, None)
            .await
            .unwrap();
        hub.send_json(json!({ "sequence": 1 })).await.unwrap();

        let result = tokio::time::timeout(
            Duration::from_millis(10),
            hub.send_json(json!({ "sequence": 2 })),
        )
        .await
        .expect("full Bridge queue must not suspend")
        .unwrap_err();

        assert_eq!(result.code, ErrorCode::BrowserUnavailable);
        assert!(result.message.contains("写入队列不可用"));
    }
}
