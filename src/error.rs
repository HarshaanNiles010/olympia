use axum::{
    Json,
    extract::multipart::MultipartError,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::convert::ConvertError;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("invalid multipart body: {0}")]
    Multipart(#[from] MultipartError),
    #[error("multipart body has no file field")]
    MissingFile,
    #[error(transparent)]
    Convert(#[from] ConvertError),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

impl AppError {
    fn status(&self) -> StatusCode {
        match self {
            AppError::Multipart(e) => e.status(),
            AppError::MissingFile => StatusCode::BAD_REQUEST,
            AppError::Convert(e) if e.is_internal() => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::Convert(e) if e.is_unsupported() => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            AppError::Convert(_) => StatusCode::UNPROCESSABLE_ENTITY,
            AppError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        // Server faults are logged in full but never leaked to the client.
        let message = if status.is_server_error() {
            tracing::error!(error = ?self, "request failed");
            "internal error".to_string()
        } else {
            tracing::warn!(error = %self, "request rejected");
            self.to_string()
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_errors_to_status() {
        let cases = [
            (AppError::MissingFile, StatusCode::BAD_REQUEST),
            (ConvertError::Unsupported("exe".into()).into(), StatusCode::UNSUPPORTED_MEDIA_TYPE),
            (ConvertError::NoExtension.into(), StatusCode::UNSUPPORTED_MEDIA_TYPE),
            (ConvertError::NoText.into(), StatusCode::UNPROCESSABLE_ENTITY),
            (ConvertError::TooManyPages(100).into(), StatusCode::UNPROCESSABLE_ENTITY),
            (ConvertError::Timeout("pandoc").into(), StatusCode::UNPROCESSABLE_ENTITY),
            (ConvertError::Tool("pandoc", "bad".into()).into(), StatusCode::UNPROCESSABLE_ENTITY),
            (ConvertError::Io(std::io::ErrorKind::NotFound.into()).into(), StatusCode::INTERNAL_SERVER_ERROR),
            (sqlx::Error::PoolTimedOut.into(), StatusCode::INTERNAL_SERVER_ERROR),
        ];
        for (err, want) in cases {
            assert_eq!(err.status(), want, "{err:?}");
        }
    }
}
