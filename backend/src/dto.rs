//! Request shapes and the small serde defaults they share.
//!
//! Both planes accept the same query string and the same publish envelope with
//! one difference (a skill carries its display `name`, an agent reads it from
//! its manifest), so the types live here once instead of being re-declared per
//! plane. `one`/`twenty`/`fifty` and `page_offset` used to be copy-pasted into
//! three modules; a default that drifts between planes is a silent API change.

use serde::Deserialize;

use crate::error::ApiError;

pub(crate) fn one() -> i64 {
    1
}

pub(crate) fn twenty() -> i64 {
    20
}

pub(crate) fn fifty() -> i64 {
    50
}

pub(crate) fn default_visibility() -> String {
    "public".to_string()
}

/// Turn a page number into an offset, or reject it. `page` comes straight from
/// the query string: `(page - 1) * limit` panics on overflow in debug and wraps
/// to a negative offset in release, where SQLite reads it as "no OFFSET" and
/// silently serves the first page instead of a 400.
pub(crate) fn page_offset(page: i64, limit: i64) -> Result<i64, ApiError> {
    if page < 1 {
        return Err(ApiError::bad_request("invalid_page", "页码必须从 1 开始"));
    }
    (page - 1)
        .checked_mul(limit)
        .ok_or_else(|| ApiError::bad_request("invalid_page", "页码过大，无法计算偏移量"))
}

/// Query string shared by both market listings and both admin listings.
#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default = "one")]
    pub page: i64,
    #[serde(default = "twenty")]
    pub limit: i64,
    /// `created` (default) | `downloads` | `updated`.
    #[serde(default)]
    pub sort: String,
}

/// `?names=a,b` — comma-separated display names, as the QwenPaw host reports
/// them. Shared by both lookup endpoints.
#[derive(Debug, Deserialize)]
pub struct LookupQuery {
    #[serde(default)]
    pub names: String,
}
