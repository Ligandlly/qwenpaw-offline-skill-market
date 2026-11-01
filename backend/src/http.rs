//! Response shaping for the one endpoint that serves megabytes: a bundle.
//!
//! A version is immutable, so its payload can be cached forever and revalidated
//! for free — the `sha256` the publish already computed is the strongest
//! possible validator. Bundles are also the only large responses here (JSON with
//! base64 blobs), so they are the only ones worth compressing.

use axum::body::Body;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use serde_json::Value;

use crate::error::{ApiError, ApiResult};

/// Below this, gzip's header and CPU cost more than it saves.
const COMPRESSION_THRESHOLD: usize = 1024;

/// One year. A version never changes, so a cached copy never goes stale.
const IMMUTABLE_MAX_AGE: u64 = 31_536_000;

/// The bundle response: `ETag` from the version's digest, cache directives that
/// match who may see it, and gzip when the client asked for it.
///
/// `shared_cache` is true only for a `public` entry: a tenant-private bundle must
/// not land in a shared cache, where another tenant could read it.
pub(crate) fn immutable_json(
    body: &Value,
    digest: &str,
    shared_cache: bool,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    let etag = etag_of(digest);
    let cache_control = if shared_cache {
        format!("public, max-age={IMMUTABLE_MAX_AGE}, immutable")
    } else {
        "private, max-age=0, must-revalidate".to_string()
    };

    let mut builder = Response::builder()
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, cache_control)
        // The payload differs per tenant (visibility) and per encoding.
        .header(header::VARY, "X-Tenant-Id, Accept-Encoding");

    if is_revalidated(headers, digest) {
        return builder
            .status(StatusCode::NOT_MODIFIED)
            .body(Body::empty())
            .map_err(bad_headers);
    }

    let mut bytes = serde_json::to_vec(body).map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            format!("响应无法序列化：{err}"),
        )
    })?;
    let encoded = bytes.len() >= COMPRESSION_THRESHOLD && accepts_gzip(headers);
    if encoded {
        bytes = gzip(&bytes)?;
        builder = builder.header(header::CONTENT_ENCODING, "gzip");
    }
    builder
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(bytes))
        .map_err(bad_headers)
}

/// Whether the client already has this exact version.
///
/// Callers use it to keep a revalidation from counting as an install: the
/// download counter is the only number a publisher sees, and a client that
/// re-checks its cache did not install anything.
pub(crate) fn is_revalidated(headers: &HeaderMap, digest: &str) -> bool {
    let Some(asked) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let wanted = etag_of(digest);
    asked.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*" || candidate == wanted || candidate.strip_prefix("W/") == Some(&wanted)
    })
}

fn etag_of(digest: &str) -> String {
    format!("\"{digest}\"")
}

fn bad_headers(err: axum::http::Error) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        format!("响应头无效：{err}"),
    )
}

/// `Accept-Encoding: gzip`, honouring an explicit refusal (`q=0`).
fn accepts_gzip(headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    value.split(',').any(|part| {
        let mut fields = part.split(';').map(str::trim);
        let coding = fields.next().unwrap_or_default();
        let refused = fields.any(|field| {
            field
                .strip_prefix("q=")
                .and_then(|q| q.parse::<f32>().ok())
                .is_some_and(|q| q <= 0.0)
        });
        coding.eq_ignore_ascii_case("gzip") && !refused
    })
}

fn gzip(bytes: &[u8]) -> ApiResult<Vec<u8>> {
    use std::io::Write;

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let failed = |err: std::io::Error| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            format!("响应无法压缩：{err}"),
        )
    };
    encoder.write_all(bytes).map_err(failed)?;
    encoder.finish().map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn refuses_gzip_when_the_client_says_no() {
        assert!(accepts_gzip(&headers(&[(
            "accept-encoding",
            "gzip, deflate"
        )])));
        assert!(accepts_gzip(&headers(&[(
            "accept-encoding",
            "deflate, GZIP"
        )])));
        assert!(!accepts_gzip(&headers(&[("accept-encoding", "gzip;q=0")])));
        assert!(!accepts_gzip(&headers(&[("accept-encoding", "deflate")])));
        assert!(!accepts_gzip(&headers(&[])));
    }

    #[test]
    fn matches_strong_and_weak_validators() {
        let digest = "abc123";
        for value in ["\"abc123\"", "W/\"abc123\"", "*", "\"other\", \"abc123\""] {
            assert!(
                is_revalidated(&headers(&[("if-none-match", value)]), digest),
                "{value}"
            );
        }
        for value in ["\"abc124\"", "", "abc123"] {
            assert!(
                !is_revalidated(&headers(&[("if-none-match", value)]), digest),
                "{value}"
            );
        }
        assert!(!is_revalidated(&headers(&[]), digest));
    }
}
