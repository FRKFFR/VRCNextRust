//! Rate-limiter and retry-path tests against `wiremock`, using tokio's paused
//! clock so no real time passes and no live network is touched (RULES.md §6).

use std::time::Duration;

use secrecy::SecretString;
use tokio::time::Instant;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use vrc_api::{ApiError, Client, MemoryVault};

const CURRENT_USER: &str = include_str!("fixtures/current_user.json");

fn ok_user() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(CURRENT_USER, "application/json")
}

fn test_client(server: &MockServer, per_second: f64, burst: u32) -> Client {
    Client::builder()
        .base_url(server.uri())
        .vault(std::sync::Arc::new(MemoryVault::new()))
        .rate_limit(per_second, burst)
        .build()
        .unwrap()
}

async fn request_count(server: &MockServer) -> usize {
    server.received_requests().await.unwrap().len()
}

#[tokio::test(start_paused = true)]
async fn token_bucket_throttles_to_one_per_second() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(ok_user())
        .mount(&server)
        .await;

    // Documented default: 1 request/second, burst 3.
    let client = Client::builder().base_url(server.uri()).build().unwrap();
    let start = Instant::now();
    for _ in 0..4 {
        client.current_user().await.unwrap();
    }
    let elapsed = start.elapsed();
    assert_eq!(request_count(&server).await, 4);
    assert!(
        elapsed >= Duration::from_secs(1),
        "4th request should wait ~1s, elapsed {elapsed:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn retries_on_5xx_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(ok_user())
        .mount(&server)
        .await;

    let client = test_client(&server, 1_000.0, 100);
    let user = client.current_user().await.unwrap();
    assert_eq!(user.display_name, "Test User");
    assert_eq!(request_count(&server).await, 3);
}

#[tokio::test(start_paused = true)]
async fn gives_up_after_capped_retries_on_5xx() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(ResponseTemplate::new(503).set_body_json(
            serde_json::json!({ "error": { "message": "synthetic outage", "status_code": 503 } }),
        ))
        .mount(&server)
        .await;

    let client = test_client(&server, 1_000.0, 100);
    let err = client.current_user().await.unwrap_err();
    match err {
        ApiError::Http { status, message } => {
            assert_eq!(status, 503);
            assert_eq!(message, "synthetic outage");
        }
        other => panic!("expected Http error, got {other:?}"),
    }
    // 1 initial attempt + 3 retries.
    assert_eq!(request_count(&server).await, 4);
}

#[tokio::test(start_paused = true)]
async fn client_errors_are_never_retried() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(serde_json::json!({ "error": { "message": "synthetic bad request", "status_code": 400 } })),
        )
        .mount(&server)
        .await;

    let client = test_client(&server, 1_000.0, 100);
    let err = client.current_user().await.unwrap_err();
    assert!(matches!(err, ApiError::Http { status: 400, .. }));
    assert_eq!(request_count(&server).await, 1);
}

#[tokio::test(start_paused = true)]
async fn honors_retry_after_and_pauses_all_requests() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "2"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(ok_user())
        .mount(&server)
        .await;

    // Generous burst: the 2 s gap must come from the 429 pause, not the bucket.
    let client = test_client(&server, 1_000.0, 100);
    let start = Instant::now();
    client.current_user().await.unwrap();
    assert!(
        start.elapsed() >= Duration::from_secs(2),
        "must honour Retry-After, elapsed {:?}",
        start.elapsed()
    );
    assert_eq!(request_count(&server).await, 2);
}

#[tokio::test(start_paused = true)]
async fn persistent_429_returns_rate_limited() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
        .mount(&server)
        .await;

    let client = test_client(&server, 1_000.0, 100);
    let err = client.current_user().await.unwrap_err();
    match err {
        ApiError::RateLimited { retry_after } => {
            assert_eq!(retry_after, Some(Duration::from_secs(1)));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
    assert_eq!(request_count(&server).await, 4);
}

#[tokio::test(start_paused = true)]
async fn network_failures_retry_within_cap() {
    // Bind then release a socket so nothing listens on the port: every
    // connect fails fast with ECONNREFUSED.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let client = Client::builder()
        .base_url(format!("http://127.0.0.1:{port}"))
        .rate_limit(1_000.0, 100)
        .build()
        .unwrap();

    let err = client.current_user().await.unwrap_err();
    assert!(matches!(err, ApiError::Network(_)));
}

#[tokio::test]
async fn every_request_sends_user_agent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .and(wiremock::matchers::header(
            "user-agent",
            vrc_api::USER_AGENT,
        ))
        .respond_with(ok_user())
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server, 1_000.0, 100);
    client.current_user().await.unwrap();
    server.verify().await;
}

#[tokio::test(start_paused = true)]
async fn post_goes_through_same_send_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/twofactorauth/totp/verify"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/twofactorauth/totp/verify"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "verified": false })),
        )
        .mount(&server)
        .await;

    let client = test_client(&server, 1_000.0, 100);
    let err = client
        .verify_2fa(
            vrc_api::TwoFactorMethod::Totp,
            &SecretString::from("000000"),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Requires2fa));
    assert_eq!(request_count(&server).await, 2);
}
