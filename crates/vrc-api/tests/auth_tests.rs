//! Auth-flow tests against a `wiremock` server with synthetic fixtures only
//! (RULES.md §6). No live network, no real credentials.

use std::sync::Arc;

use secrecy::SecretString;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

use vrc_api::{
    ApiError, Client, CookieVault, LoginOutcome, MemoryVault, TwoFactorMethod, USER_AGENT,
};

const CURRENT_USER: &str = include_str!("fixtures/current_user.json");
const FAKE_COOKIE: &str = "authcookie_00000000000000000000000000000000";

/// Decode standard base64 (no padding required) so tests can assert on the
/// documented `Basic base64(urlencode(u):urlencode(p))` scheme.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    const INVALID: u8 = 0xff;
    let mut table = [INVALID; 256];
    for (i, b) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
        .iter()
        .enumerate()
    {
        table[*b as usize] = i as u8;
    }
    let bytes: Vec<u8> = input.bytes().filter(|b| *b != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut acc = 0u32;
        for (i, b) in chunk.iter().enumerate() {
            let v = table[*b as usize];
            if v == INVALID {
                return None;
            }
            acc |= (v as u32) << (18 - 6 * i);
        }
        for i in 0..(chunk.len() - 1) {
            out.push((acc >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

/// Asserts `Authorization: Basic base64(encoded_user:encoded_pass)`.
struct BasicAuth {
    user: &'static str,
    pass: &'static str,
}

impl Match for BasicAuth {
    fn matches(&self, request: &Request) -> bool {
        let Some(value) = request.headers.get("authorization") else {
            return false;
        };
        let Some(encoded) = value
            .to_str()
            .ok()
            .and_then(|v| v.strip_prefix("Basic "))
            .and_then(base64_decode)
            .and_then(|raw| String::from_utf8(raw).ok())
        else {
            return false;
        };
        encoded == format!("{}:{}", self.user, self.pass)
    }
}

fn test_vault() -> Arc<MemoryVault> {
    Arc::new(MemoryVault::new())
}

fn test_client(server: &MockServer, vault: Arc<MemoryVault>) -> Client {
    Client::builder()
        .base_url(server.uri())
        .vault(vault)
        .rate_limit(1_000.0, 100)
        .build()
        .expect("client builds")
}

fn current_user_response() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_raw(CURRENT_USER, "application/json")
        .insert_header(
            "set-cookie",
            format!("auth={FAKE_COOKIE}; Path=/; HttpOnly"),
        )
}

#[tokio::test]
async fn login_needs_2fa_lists_methods_and_uses_documented_basic_auth() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .and(header("user-agent", USER_AGENT))
        .and(BasicAuth {
            // `test user+1@example.com` url-encoded per the API docs.
            user: "test%20user%2B1%40example.com",
            pass: "synthetic%20password",
        })
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "requiresTwoFactorAuth": ["totp", "emailOtp"] })),
        )
        .mount(&server)
        .await;

    let client = test_client(&server, test_vault());
    let outcome = client
        .login(
            "test user+1@example.com",
            &SecretString::from("synthetic password"),
        )
        .await
        .unwrap();

    match outcome {
        LoginOutcome::Needs2fa(methods) => {
            assert_eq!(
                methods,
                vec![TwoFactorMethod::Totp, TwoFactorMethod::EmailOtp]
            );
        }
        LoginOutcome::LoggedIn(_) => panic!("expected Needs2fa"),
    }
    server.verify().await;
}

#[tokio::test]
async fn login_full_session_persists_auth_cookie_in_vault() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(current_user_response())
        .mount(&server)
        .await;

    let vault = test_vault();
    let client = test_client(&server, vault.clone());
    let outcome = client
        .login("testuser", &SecretString::from("synthetic password"))
        .await
        .unwrap();

    let LoginOutcome::LoggedIn(user) = outcome else {
        panic!("expected LoggedIn")
    };
    assert_eq!(user.id, "usr_00000000-0000-0000-0000-000000000001");
    assert_eq!(user.display_name, "Test User");
    assert_eq!(
        vault.load().unwrap().as_deref(),
        Some(format!("auth={FAKE_COOKIE}").as_str())
    );
}

#[tokio::test]
async fn auth_token_returns_secret_auth_cookie_value() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(current_user_response())
        .mount(&server)
        .await;

    let client = test_client(&server, test_vault());
    client
        .login("testuser", &SecretString::from("pw"))
        .await
        .unwrap();

    let token = client.auth_token().expect("auth token present");
    use secrecy::ExposeSecret;
    assert_eq!(token.expose_secret(), FAKE_COOKIE);
    // Debug must redact the secret (RULES.md §4).
    assert!(!format!("{token:?}").contains(FAKE_COOKIE));
}

#[tokio::test]
async fn verify_2fa_persists_cookies_and_returns_current_user() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/twofactorauth/emailotp/verify"))
        .and(body_json(json!({ "code": "000000" })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "verified": true }))
                .insert_header("set-cookie", format!("auth={FAKE_COOKIE}; Path=/")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(CURRENT_USER, "application/json"))
        .mount(&server)
        .await;

    let vault = test_vault();
    let client = test_client(&server, vault.clone());
    let user = client
        .verify_2fa(TwoFactorMethod::EmailOtp, &SecretString::from("000000"))
        .await
        .unwrap();

    assert_eq!(user.display_name, "Test User");
    assert_eq!(
        vault.load().unwrap().as_deref(),
        Some(format!("auth={FAKE_COOKIE}").as_str())
    );
}

#[tokio::test]
async fn verify_2fa_rejected_reports_requires_2fa() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/twofactorauth/totp/verify"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "verified": false })))
        .mount(&server)
        .await;

    let client = test_client(&server, test_vault());
    let err = client
        .verify_2fa(TwoFactorMethod::Totp, &SecretString::from("000000"))
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Requires2fa));
}

#[tokio::test]
async fn bad_credentials_return_unauthorized_and_clear_vault() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(ResponseTemplate::new(401).set_body_json(
            json!({ "error": { "message": "\"Invalid Credentials\"", "status_code": 401 } }),
        ))
        .mount(&server)
        .await;

    let vault = test_vault();
    vault.save("auth=stale_cookie").unwrap();
    let client = test_client(&server, vault.clone());
    let err = client
        .login("testuser", &SecretString::from("wrong"))
        .await
        .unwrap_err();

    assert!(matches!(err, ApiError::Unauthorized));
    assert_eq!(vault.load().unwrap(), None);
}

#[tokio::test]
async fn resume_session_without_vault_or_cookie_is_none() {
    let server = MockServer::start().await;
    let client = Client::builder()
        .base_url(server.uri())
        .rate_limit(1_000.0, 100)
        .build()
        .unwrap();
    assert_eq!(client.resume_session().await.unwrap(), None);

    let client = test_client(&server, test_vault());
    assert_eq!(client.resume_session().await.unwrap(), None);
}

#[tokio::test]
async fn resume_session_replays_vault_cookies() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .and(header("cookie", format!("auth={FAKE_COOKIE}")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(CURRENT_USER, "application/json"))
        .mount(&server)
        .await;

    let vault = test_vault();
    vault.save(&format!("auth={FAKE_COOKIE}")).unwrap();
    let client = test_client(&server, vault);

    let user = client
        .resume_session()
        .await
        .unwrap()
        .expect("session resumes");
    assert_eq!(user.id, "usr_00000000-0000-0000-0000-000000000001");
    server.verify().await;
}

#[tokio::test]
async fn resume_session_expired_cookie_clears_vault_and_is_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let vault = test_vault();
    vault.save(&format!("auth={FAKE_COOKIE}")).unwrap();
    let client = test_client(&server, vault.clone());

    assert_eq!(client.resume_session().await.unwrap(), None);
    assert_eq!(vault.load().unwrap(), None);
}

#[tokio::test]
async fn current_user_reports_requires_2fa_when_pending() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "requiresTwoFactorAuth": ["totp"] })),
        )
        .mount(&server)
        .await;

    let client = test_client(&server, test_vault());
    let err = client.current_user().await.unwrap_err();
    assert!(matches!(err, ApiError::Requires2fa));
}

#[tokio::test]
async fn logout_uses_put_and_clears_vault_and_cookies() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(current_user_response())
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/logout"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "success": { "message": "Ok!", "status_code": 200 } })),
        )
        .mount(&server)
        .await;

    let vault = test_vault();
    let client = test_client(&server, vault.clone());
    client
        .login("testuser", &SecretString::from("pw"))
        .await
        .unwrap();
    assert!(client.auth_token().is_some());

    client.logout().await.unwrap();
    assert_eq!(vault.load().unwrap(), None);
    assert!(client.auth_token().is_none());
    server.verify().await;
}

#[tokio::test]
async fn logout_on_expired_session_still_clears() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/logout"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let vault = test_vault();
    vault.save("auth=stale").unwrap();
    let client = test_client(&server, vault.clone());
    client.logout().await.unwrap();
    assert_eq!(vault.load().unwrap(), None);
}

/// The keyring vault talks to the real OS credential store — opt-in only.
#[test]
#[ignore]
fn keyring_vault_round_trip() {
    let vault = vrc_api::KeyringVault::new().unwrap();
    vault.clear().unwrap();
    assert_eq!(vault.load().unwrap(), None);
    vault.save("auth=synthetic_cookie").unwrap();
    assert_eq!(
        vault.load().unwrap().as_deref(),
        Some("auth=synthetic_cookie")
    );
    vault.clear().unwrap();
}
