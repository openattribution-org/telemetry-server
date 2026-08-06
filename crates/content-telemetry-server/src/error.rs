use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),

    #[error("missing or malformed {0} header")]
    MissingOrg(&'static str),

    #[error("not found")]
    NotFound,

    #[error("database error")]
    Database(#[from] sqlx::Error),
}

impl ApiError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            Self::MissingOrg(_) => (StatusCode::UNAUTHORIZED, self.to_string()),
            Self::NotFound => (StatusCode::NOT_FOUND, self.to_string()),
            // Never surface the driver's message: it can carry query text and
            // column values.
            Self::Database(e) => {
                tracing::error!(error = %e, "database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_string(),
                )
            }
        };

        (status, Json(json!({ "error": message }))).into_response()
    }
}
