use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unauthorized,
    InvalidRequest,
    BrowserUnavailable,
    QueueFull,
    Timeout,
    NavigationFailed,
    ExtractionFailed,
    Cancelled,
    ProtocolError,
    InstanceConflict,
    InternalError,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDetail {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

impl ErrorDetail {
    pub fn new(code: ErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidRequest, message, false)
    }

    pub fn browser_unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BrowserUnavailable, message, true)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Timeout, message, true)
    }
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: ErrorDetail,
}

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub detail: ErrorDetail,
}

impl ApiError {
    pub fn new(status: StatusCode, detail: ErrorDetail) -> Self {
        Self { status, detail }
    }

    pub fn from_detail(detail: ErrorDetail) -> Self {
        let status = match detail.code {
            ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
            ErrorCode::InvalidRequest | ErrorCode::ProtocolError => StatusCode::BAD_REQUEST,
            ErrorCode::BrowserUnavailable | ErrorCode::InstanceConflict => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            ErrorCode::QueueFull => StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::Timeout => StatusCode::GATEWAY_TIMEOUT,
            ErrorCode::Cancelled => StatusCode::CONFLICT,
            ErrorCode::NavigationFailed | ErrorCode::ExtractionFailed => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            ErrorCode::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self { status, detail }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(ErrorResponse { error: self.detail })).into_response()
    }
}
