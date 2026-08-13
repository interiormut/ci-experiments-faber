use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use thiserror::Error;

#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum AppError {
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),

    #[error("not found")]
    NotFound,

    #[error("bad request: {0}")]
    BadRequest(String),

    /// A well-formed request against a row whose state does not admit it —
    /// stopping a run that already finished. Distinct from `BadRequest`
    /// because nothing about the request was wrong: the same bytes sent a
    /// moment earlier would have worked, and a client can tell "retry is
    /// pointless" from "fix your input".
    #[error("conflict: {0}")]
    Conflict(String),

    #[error("database error: {source}")]
    Db {
        source: diesel::result::Error,
        context: Option<&'static str>,
    },

    #[error("pool error: {0}")]
    Pool(#[from] diesel_async::pooled_connection::bb8::RunError),

    #[error("upstream request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// A harness run that ended in a failure the harness did not handle —
    /// almost always the provider, occasionally the harness's own code. The
    /// message is carried rather than flattened to "internal error": this
    /// reaches a user as a `run_error` on their stream, and "the provider
    /// refused the key" and "faber is broken" are not the same news.
    #[error("{0}")]
    Harness(String),

    #[error("internal error")]
    Internal,
}

impl AppError {
    pub fn db(source: diesel::result::Error, context: &'static str) -> Self {
        Self::Db {
            source,
            context: Some(context),
        }
    }

    fn log_response_error(&self, status: StatusCode) {
        if !status.is_server_error() {
            return;
        }

        match self {
            AppError::Db { source, context } => log_database_error(status, source, *context),
            AppError::Pool(source) => {
                tracing::error!(
                    status = %status,
                    error = %source,
                    error_debug = ?source,
                    "request failed while acquiring a database connection"
                );
            }
            AppError::Http(source) => {
                tracing::error!(
                    status = %status,
                    error = %source,
                    error_debug = ?source,
                    "request failed during upstream HTTP call"
                );
            }
            AppError::Internal => {
                tracing::error!(status = %status, "request failed with internal error");
            }
            // Already logged in full at the run that produced it.
            AppError::Harness(_) => {}
            AppError::Unauthorized(_)
            | AppError::ServiceUnavailable(_)
            | AppError::NotFound
            | AppError::BadRequest(_)
            | AppError::Conflict(_) => {}
        }
    }
}

impl From<diesel::result::Error> for AppError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Db {
            source,
            context: None,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg): (StatusCode, String) = match &self {
            AppError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "unauthorized".to_owned()),
            AppError::ServiceUnavailable(msg) => (StatusCode::SERVICE_UNAVAILABLE, msg.clone()),
            AppError::NotFound => (StatusCode::NOT_FOUND, "not found".to_owned()),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg.clone()),
            AppError::Db { source, .. } => {
                use diesel::result::Error as De;
                match source {
                    De::NotFound => (StatusCode::NOT_FOUND, "not found".to_owned()),
                    _ => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "database error".to_owned(),
                    ),
                }
            }
            AppError::Pool(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "service unavailable".to_owned(),
            ),
            AppError::Http(_) => (StatusCode::BAD_GATEWAY, "upstream error".to_owned()),
            AppError::Harness(msg) => (StatusCode::BAD_GATEWAY, msg.clone()),
            AppError::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal error".to_owned(),
            ),
        };
        self.log_response_error(status);
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

fn log_database_error(
    status: StatusCode,
    source: &diesel::result::Error,
    context: Option<&'static str>,
) {
    use diesel::result::Error as DieselError;

    match source {
        DieselError::DatabaseError(kind, info) => {
            tracing::error!(
                status = %status,
                context,
                db_error_kind = ?kind,
                db_message = %info.message(),
                db_details = ?info.details(),
                db_hint = ?info.hint(),
                db_table = ?info.table_name(),
                db_column = ?info.column_name(),
                db_constraint = ?info.constraint_name(),
                error = %source,
                error_debug = ?source,
                "request failed with database error"
            );
        }
        other => {
            tracing::error!(
                status = %status,
                context,
                error = %other,
                error_debug = ?other,
                "request failed with database error"
            );
        }
    }
}

pub type ApiResult<T> = Result<T, AppError>;
