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
        let payload: Vec<u8> = (0..bytes).map(|offset| (offset % 251) as u8 | 0x80).collect();
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
