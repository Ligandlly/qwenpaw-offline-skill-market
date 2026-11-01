//! The one error type every handler returns.
//!
//! It lives in its own module because it is *not* a tenant-API detail: `auth`
//! rejects bad tenants with it, `admin` rejects bad sessions with it, `stats`
//! and `agents` use it too. Keeping it inside `api` made every other module
//! depend on the tenant plane, which is backwards (and the reason `admin` grew
//! a duplicate `internal` helper).

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use tracing::error;

use crate::limits::Reject;

/// An error with a machine-readable `code` and a human `message`.
///
/// The body is always `{"error": {"code": ..., "message": ...}}`: the QwenPaw
/// plugin and the admin page both switch on `code`, so the shape is part of the
/// API contract (see `tests/contract.rs`).
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn not_found(what: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", what)
    }

    pub fn db(err: rusqlite::Error) -> Self {
        error!(error = %err, "database error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            format!("数据库错误：{err}"),
        )
    }

    /// A `query_row` that matched nothing means "not found"; any other error
    /// means the database is broken. Collapsing both into 404 turns an outage
    /// into "your package was deleted", so the distinction is load-bearing.
    pub fn query_miss(err: rusqlite::Error, what: &str) -> Self {
        match err {
            rusqlite::Error::QueryReturnedNoRows => Self::not_found(what),
            other => Self::db(other),
        }
    }

    /// Same mapping, but the miss answers 410 `gone`: the entry used to exist.
    pub fn query_gone(err: rusqlite::Error, what: &str) -> Self {
        match err {
            rusqlite::Error::QueryReturnedNoRows => Self::new(StatusCode::GONE, "gone", what),
            other => Self::db(other),
        }
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    /// Our own bug or a broken database: log the detail, tell the caller
    /// nothing specific.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }
}

impl From<Reject> for ApiError {
    fn from(reject: Reject) -> Self {
        Self::new(StatusCode::BAD_REQUEST, reject.code, reject.message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": { "code": self.code, "message": self.message } })),
        )
            .into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
