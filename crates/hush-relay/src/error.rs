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
        }
    }
}

impl IntoResponse for RelayError {
    fn into_response(self) -> Response {
        let body = Json(json!({ "error": self.to_string() }));
        (self.status(), body).into_response()
    }
}
