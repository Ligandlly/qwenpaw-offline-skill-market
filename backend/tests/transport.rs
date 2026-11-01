//! Transport guards: how much a single request may carry, and how long it may
//! take. Both are limits of the HTTP layer, not of the market, so they are
//! asserted separately from the publish contract.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware;
use axum::routing::get;
use axum::Router;
use base64ct::{Base64, Encoding};
use common::{test_config, Harness, ADMIN_USER};
use serde_json::{json, Value};
use std::time::Duration;
use tower::ServiceExt;

fn binary_bundle(files: usize, bytes: usize) -> Value {
    let mut bundle = serde_json::Map::new();
    for index in 0..files {
        let payload: Vec<u8> = (0..bytes)
            .map(|offset| (offset % 251) as u8 | 0x80)
            .collect();
        bundle.insert(
            format!("assets/{index}.bin"),
            json!({ "base64": Base64::encode_string(&payload) }),
        );
    }
    serde_json::Value::Object(bundle)
}

/// The old global 64 MiB cap let anything through; axum's own 2 MiB default
/// would reject a legitimate bundle of binary files, whose JSON form is 4/3 of
/// the raw size. The publish routes must sit between the two.
#[tokio::test]
async fn a_legit_big_bundle_is_not_cut_by_the_transport_limit() {
    let h = Harness::new();
    // 7 x 250 KiB = 1.75 MiB of raw bytes, inside the 2 MiB market quota, but
    // about 2.33 MiB once base64-encoded: past axum's default body limit.
    let mut files = binary_bundle(7, 250_000);
    files["SKILL.md"] = json!(common::GOOD_MANIFEST);
    let body = json!({ "name": "Big Skill", "version": "1.0.0", "files": files });
    let (status, answer) = h.publish_skill_with("tenant-a", body).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    assert_eq!(
        answer["size_bytes"],
        1_750_000 + common::GOOD_MANIFEST.len(),
        "{answer}"
    );
}

#[tokio::test]
async fn an_absurd_body_is_refused_before_it_is_parsed() {
    let h = Harness::new();
    let body = json!({
        "name": "Huge",
        "version": "1.0.0",
        "files": binary_bundle(1, 20 * 1024 * 1024),
    });
    let (status, _) = h.publish_skill_with("tenant-a", body).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

    // The sign-in form carries two short strings; a megabyte of "password" is
    // refused by the transport, not hashed.
    let (status, _, _) = h
        .call(
            "POST",
            "/admin/api/login",
            None,
            Some(json!({ "username": ADMIN_USER, "password": "x".repeat(1024 * 1024) })),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn a_stalled_request_becomes_a_json_error_instead_of_hanging() {
    let mut cfg = test_config();
    cfg.request_timeout = Duration::from_millis(20);
    let h = Harness::with_config(cfg);
    // The real router only has fast routes, so probe the deadline with a
    // deliberately slow one behind the same middleware.
    let probe = Router::new()
        .route(
            "/deep/probe",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(500)).await;
                "late"
            }),
        )
        .layer(middleware::from_fn_with_state(
            h.state.clone(),
            offline_market_backend::budget_request,
        ))
        .with_state(h.state.clone());

    let response = h
        .app
        .clone()
        .merge(probe)
        .oneshot(
            Request::builder()
                .uri("/deep/probe")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let answer: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(answer["error"]["code"], "timeout", "{answer}");
}

/// Database work runs on the blocking pool, so a panic inside a query must
/// surface as one failed request — not as a dead runtime worker.
#[tokio::test]
async fn a_panicking_query_fails_one_request_not_the_runtime() {
    let h = Harness::new();
    let result: Result<(), _> =
        offline_market_backend::db::offload(&h.state, |_app| panic!("boom")).await;
    let err = result.expect_err("a panicking query must not come back as Ok");
    assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(err.code, "internal");
    // Still serving: the runtime survived the panic.
    let (status, body, _) = h
        .call("GET", "/api/v1/skills", Some("alpha"), None, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// A published version never changes, so its payload is cacheable forever and
/// revalidating it is free: the digest the publish computed is the validator.
#[tokio::test]
async fn bundle_responses_revalidate_on_their_digest() {
    let h = Harness::new();
    let (status, body) = h.publish_skill("tenant-a", "demo", "1.0.0", "public").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, headers, bytes) = h
        .call_headers("/skills/demo/1.0.0", Some("tenant-b"), &[])
        .await;
    assert_eq!(status, StatusCode::OK);
    let etag = headers
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .expect("a bundle must carry an ETag")
        .to_string();
    assert!(etag.starts_with('"') && etag.ends_with('"'), "{etag}");
    assert_eq!(
        headers
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("public, max-age=31536000, immutable"),
        "a public immutable payload may be cached by anything"
    );
    assert!(!bytes.is_empty());

    // The client that already has it asks again and pays for headers only.
    let (status, headers, bytes) = h
        .call_headers(
            "/skills/demo/1.0.0",
            Some("tenant-b"),
            &[("if-none-match", &etag)],
        )
        .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert!(bytes.is_empty(), "a 304 must not carry a body");
    assert_eq!(
        headers.get("etag").and_then(|value| value.to_str().ok()),
        Some(etag.as_str())
    );

    // The revalidation did not count as an install: the counter is what a
    // publisher reads, and a client checking its cache installed nothing.
    h.flush();
    assert_eq!(h.stored_downloads("skills", "demo"), 1);

    // A stale validator still gets the payload, and does count.
    let (status, _, bytes) = h
        .call_headers(
            "/skills/demo/1.0.0",
            Some("tenant-b"),
            &[("if-none-match", "\"not-the-digest\"")],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!bytes.is_empty());
    h.flush();
    assert_eq!(h.stored_downloads("skills", "demo"), 2);

    // A tenant-private bundle must never reach a shared cache.
    let (status, body) = h
        .publish_skill("tenant-a", "other", "1.0.0", "tenant")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, headers, _) = h
        .call_headers("/skills/other/1.0.0", Some("tenant-a"), &[])
        .await;
    assert_eq!(status, StatusCode::OK);
    let cache_control = headers
        .get("cache-control")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(cache_control.starts_with("private"), "{cache_control}");
    assert!(!cache_control.contains("public"), "{cache_control}");
}

/// Bundles are JSON with base64 payloads — the one response where compression is
/// worth its CPU.
#[tokio::test]
async fn big_bundles_are_compressed_for_clients_that_ask() {
    let h = Harness::new();
    // Highly compressible text, well past the 1 KiB threshold.
    let filler = "x".repeat(50_000);
    let mut files = json!({
        "SKILL.md": common::GOOD_MANIFEST,
        "references/notes.md": filler,
    });
    let (status, body) = h
        .publish_skill_with(
            "tenant-a",
            json!({
                "name": "Demo",
                "slug": "demo",
                "version": "1.0.0",
                "visibility": "public",
                "files": files.take(),
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, headers, plain) = h
        .call_headers("/skills/demo/1.0.0", Some("tenant-b"), &[])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers.get("content-encoding").is_none(),
        "compression was not asked for"
    );

    let (status, headers, gzipped) = h
        .call_headers(
            "/skills/demo/1.0.0",
            Some("tenant-b"),
            &[("accept-encoding", "gzip, deflate")],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("content-encoding")
            .and_then(|value| value.to_str().ok()),
        Some("gzip")
    );
    assert!(
        gzipped.len() < plain.len() / 4,
        "gzip should shrink 50 KiB of `x` far more than this: {} vs {}",
        gzipped.len(),
        plain.len()
    );

    // The bytes are the same payload once decoded.
    use std::io::Read;
    let mut decoded = Vec::new();
    flate2::read::GzDecoder::new(&gzipped[..])
        .read_to_end(&mut decoded)
        .expect("gzip body");
    assert_eq!(decoded, plain.to_vec());

    // A client that refuses gzip gets the identity encoding.
    let (status, headers, bytes) = h
        .call_headers(
            "/skills/demo/1.0.0",
            Some("tenant-b"),
            &[("accept-encoding", "gzip;q=0")],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("content-encoding").is_none());
    assert_eq!(bytes.len(), plain.len());
}

/// A market whose database lost its schema must stop answering "ok": otherwise
/// the container stays healthy while every real request fails.
#[tokio::test]
async fn health_reports_a_broken_database() {
    let h = Harness::new();
    let (status, _, bytes) = h.call_headers("/health", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_slice(&bytes).expect("health json");
    assert_eq!(body["status"], "ok", "{body}");
    assert_eq!(body["database"], "ok", "{body}");

    h.exec("ALTER TABLE tenants RENAME TO tenants_gone");
    let (status, _, bytes) = h.call_headers("/health", None, &[]).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a broken schema must fail"
    );
    let body: Value = serde_json::from_slice(&bytes).expect("health json");
    assert_eq!(body["status"], "degraded", "{body}");
    assert_eq!(body["database"], "error", "{body}");

    h.exec("ALTER TABLE tenants_gone RENAME TO tenants");
    let (status, _, _) = h.call_headers("/health", None, &[]).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "health must recover with the database"
    );
}
