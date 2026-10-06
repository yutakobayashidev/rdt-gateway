use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use rdt_gateway_types::{ApiError, ErrorEnvelope};

#[derive(Debug, Clone)]
pub struct Error {
    pub status: StatusCode,
    pub body: ApiError,
}

impl Error {
    pub fn new(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ApiError {
                code: code.into(),
                message: message.into(),
                retryable: status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS,
                retry_after_seconds: None,
            },
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }
    pub fn schema(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_GATEWAY,
            "upstream_invalid_response",
            message,
        )
    }
}

impl From<rdt_request::Error> for Error {
    fn from(e: rdt_request::Error) -> Self {
        Self {
            status: StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_GATEWAY),
            body: ApiError {
                code: e.code.into(),
                message: e.message,
                retryable: e.retryable,
                retry_after_seconds: e.retry_after_seconds,
            },
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let retry = self.body.retry_after_seconds;
        let mut response = (self.status, Json(ErrorEnvelope { error: self.body })).into_response();
        if let Some(seconds) = retry {
            response
                .headers_mut()
                .insert("retry-after", seconds.to_string().parse().unwrap());
        }
        response
    }
}
