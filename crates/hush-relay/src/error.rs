use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RelayError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("queue burned")]
    Burned,
    #[error("not found")]
    NotFound,
    #[error("payload too large")]
    TooLarge,
    #[error("rate limited")]
    RateLimited {
        /// Seconds until this queue's write budget refills. Sent as
        /// `Retry-After`, because a client cannot tell a 429 that clears in a
        /// second from one that clears in a minute, and the difference decides
        /// whether waiting is worth it.
        retry_after: u64,
    },
    #[error("bad request")]
    BadRequest,
    #[error("internal")]
    Internal,
    /// The notification could not be handed to the mail transport. Distinct from
    /// `Internal` so a caller can tell "the relay broke" from "the mail server
    /// refused the message".
    #[error("mail: {0}")]
    Mail(String),
}

impl RelayError {
    fn status(&self) -> StatusCode {
        match self {
            RelayError::Unauthorized => StatusCode::UNAUTHORIZED,
            RelayError::Burned => StatusCode::FORBIDDEN,
            RelayError::NotFound => StatusCode::NOT_FOUND,
            RelayError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            RelayError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            RelayError::BadRequest => StatusCode::BAD_REQUEST,
            RelayError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            RelayError::Mail(_) => StatusCode::BAD_GATEWAY,
        }
    }
}

impl IntoResponse for RelayError {
    fn into_response(self) -> Response {
        let status = self.status();
        // A rate limit is the one refusal a client can act on by waiting, so it
        // carries how long. `Retry-After` is seconds by default, which is what
        // `retry_after` already holds.
        let retry_after = match self {
            RelayError::RateLimited { retry_after } => Some(retry_after),
            _ => None,
        };
        let body = Json(json!({ "error": self.to_string() }));
        let mut response = (status, body).into_response();
        if let Some(seconds) = retry_after {
            if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
        }
        response
    }
}
