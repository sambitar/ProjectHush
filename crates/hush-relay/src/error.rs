use axum::{
    http::StatusCode,
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
    RateLimited,
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
            RelayError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            RelayError::BadRequest => StatusCode::BAD_REQUEST,
            RelayError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            RelayError::Mail(_) => StatusCode::BAD_GATEWAY,
        }
    }
}

impl IntoResponse for RelayError {
    fn into_response(self) -> Response {
        let body = Json(json!({ "error": self.to_string() }));
        (self.status(), body).into_response()
    }
}
