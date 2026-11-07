//! M6 Discord webhook relay.
//!
//! Up to [`MAX_WEBHOOKS`] webhook URLs are stored **only** in the OS
//! credential store via [`WebhookVault`] — never in SQLite, logs, or
//! `Debug` output ([`WebhookEntry`] holds a [`SecretString`]). Sends are
//! sequential, size-capped, and go through the shared `vrc-api` client
//! so the rate limiter, retries and `Retry-After` handling apply.
//! Tests inject a memory vault and a wiremock base-URL override — no
//! real Discord traffic ever leaves the test suite.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use secrecy::{ExposeSecret, SecretString};
use time::OffsetDateTime;
use tokio::sync::mpsc;
use vrc_api::Client;

use crate::app::{AppEvent, StateHandle};

/// Maximum configured webhook URLs.
pub const MAX_WEBHOOKS: usize = 4;

/// Photos larger than this are skipped — Discord rejects oversized
/// multipart uploads anyway.
pub const MAX_PHOTO_BYTES: u64 = 16 * 1024 * 1024;

/// Keyring coordinates for the serialized webhook list.
const KEYRING_SERVICE: &str = "VRCNextRust";
const KEYRING_ACCOUNT: &str = "discord-webhooks";

/// One configured webhook: secret URL + auto-post flag.
#[derive(Clone)]
pub struct WebhookEntry {
    /// Full `https://discord.com/api/webhooks/{id}/{token}` URL.
    /// `SecretString` keeps it out of `Debug`/logs.
    pub url: SecretString,
    /// Auto-post new `PhotoTaken` events to this webhook.
    pub autopost: bool,
}

impl std::fmt::Debug for WebhookEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookEntry")
            .field("url", &"[redacted]")
            .field("autopost", &self.autopost)
            .finish()
    }
}

/// Secret store for webhook URLs. The OS-keyring implementation is the
/// production one; tests use [`MemoryWebhookVault`].
pub trait WebhookVault: Send + Sync {
    /// Load all configured entries (at most [`MAX_WEBHOOKS`]).
    fn load(&self) -> Result<Vec<WebhookEntry>, String>;
    /// Replace the stored list.
    fn save(&self, entries: &[WebhookEntry]) -> Result<(), String>;
}

/// OS credential store (`keyring` crate) — the only place webhook URLs
/// may persist (RULES.md §4 spirit: secrets never touch SQLite/logs).
pub struct KeyringWebhookVault {
    entry: keyring::Entry,
}

impl KeyringWebhookVault {
    /// Open the keyring entry; fails when no credential store exists.
    pub fn new() -> Result<Self, String> {
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
            .map(|entry| Self { entry })
            .map_err(|e| e.to_string())
    }
}

impl WebhookVault for KeyringWebhookVault {
    fn load(&self) -> Result<Vec<WebhookEntry>, String> {
        match self.entry.get_password() {
            Ok(json) => parse_webhooks_json(&json),
            Err(keyring::Error::NoEntry) => Ok(Vec::new()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn save(&self, entries: &[WebhookEntry]) -> Result<(), String> {
        let json = webhooks_json(entries)?;
        self.entry.set_password(&json).map_err(|e| e.to_string())
    }
}

/// In-memory vault for tests and vault-unavailable fallbacks.
#[derive(Default)]
pub struct MemoryWebhookVault {
    inner: Mutex<Vec<WebhookEntry>>,
}

impl MemoryWebhookVault {
    /// An empty vault.
    pub fn new() -> Self {
        Self::default()
    }

    /// A vault pre-seeded with entries.
    #[cfg(test)]
    pub fn with_entries(entries: Vec<WebhookEntry>) -> Self {
        Self {
            inner: Mutex::new(entries),
        }
    }
}

impl WebhookVault for MemoryWebhookVault {
    fn load(&self) -> Result<Vec<WebhookEntry>, String> {
        Ok(self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    fn save(&self, entries: &[WebhookEntry]) -> Result<(), String> {
        *self.inner.lock().unwrap_or_else(|e| e.into_inner()) = entries.to_vec();
        Ok(())
    }
}

fn webhooks_json(entries: &[WebhookEntry]) -> Result<String, String> {
    let list: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "url": e.url.expose_secret(),
                "autopost": e.autopost,
            })
        })
        .collect();
    serde_json::to_string(&list).map_err(|e| e.to_string())
}

fn parse_webhooks_json(json: &str) -> Result<Vec<WebhookEntry>, String> {
    let list: Vec<serde_json::Value> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    for item in list.into_iter().take(MAX_WEBHOOKS) {
        let Some(url) = item.get("url").and_then(|v| v.as_str()) else {
            continue;
        };
        // Entries written outside `add_webhook` (hand-edited keyring,
        // older builds) bypass URL validation — drop anything that isn't
        // a strict Discord webhook URL so the relay can never POST photo
        // bytes to an arbitrary host.
        if validate_webhook_url(url).is_err() {
            continue;
        }
        let autopost = item
            .get("autopost")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        entries.push(WebhookEntry {
            url: SecretString::from(url.to_string()),
            autopost,
        });
    }
    Ok(entries)
}

/// Strict webhook URL validation: `https`, host `discord.com` /
/// `discordapp.com` (or a subdomain), path `/api/webhooks/{id}/{token}`.
pub fn validate_webhook_url(raw: &str) -> Result<(), String> {
    let url = raw.trim();
    let Some(rest) = url.strip_prefix("https://") else {
        return Err("webhook URL must use https".to_string());
    };
    // authority = up to the first '/'; reject userinfo and non-default ports.
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    if authority.contains('@') || authority.contains(':') {
        return Err("webhook URL must not carry credentials or a port".to_string());
    }
    let host = authority.to_ascii_lowercase();
    let host_ok = host == "discord.com"
        || host == "discordapp.com"
        || host.ends_with(".discord.com")
        || host.ends_with(".discordapp.com");
    if !host_ok {
        return Err("webhook host must be discord.com or discordapp.com".to_string());
    }
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    // /api/webhooks/{webhook_id}/{token}
    if segments.len() < 4 || segments[0] != "api" || segments[1] != "webhooks" {
        return Err("webhook path must be /api/webhooks/{id}/{token}".to_string());
    }
    Ok(())
}

/// What the UI needs to render a webhook slot — never the URL.
#[derive(Debug, Clone, Default)]
pub struct WebhookSlot {
    /// Auto-post flag for this slot.
    pub autopost: bool,
}

/// Convert vault entries into UI-safe slots (URL never leaves the vault).
pub fn slots_for(entries: &[WebhookEntry]) -> Vec<WebhookSlot> {
    entries
        .iter()
        .map(|e| WebhookSlot {
            autopost: e.autopost,
        })
        .collect()
}

struct RelayJob {
    /// Photo file to attach.
    path: PathBuf,
    /// Only send to webhooks with `autopost` enabled.
    auto_only: bool,
}

/// Sequential webhook sender. Jobs are processed one at a time in
/// arrival order; each job posts to every configured webhook.
pub struct WebhookRelay {
    tx: mpsc::UnboundedSender<RelayJob>,
}

impl WebhookRelay {
    /// Spawn the relay loop on the current Tokio runtime.
    pub fn spawn(
        client: Client,
        vault: Arc<dyn WebhookVault>,
        state: StateHandle,
        events: tokio::sync::broadcast::Sender<AppEvent>,
    ) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<RelayJob>();
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                relay_job(&client, &vault, &state, &events, &job).await;
            }
        });
        Self { tx }
    }

    /// Queue a photo for delivery. `auto_only` restricts the send to
    /// webhooks flagged `autopost` (used by the `PhotoTaken` hook).
    pub fn send(&self, path: PathBuf, auto_only: bool) {
        let _ = self.tx.send(RelayJob { path, auto_only });
    }
}

/// Test-only hook: rewrite webhook endpoints to a mock base URL while
/// keeping the real path (`/api/webhooks/{id}/{token}`).
#[cfg(test)]
pub(crate) fn endpoint_for(webhook_url: &str, base_override: Option<&str>) -> String {
    match base_override {
        Some(base) => {
            let rest = webhook_url
                .split_once("://")
                .map(|(_, r)| r)
                .unwrap_or(webhook_url);
            let path = rest.find('/').map(|i| &rest[i..]).unwrap_or("/");
            format!("{}{}", base.trim_end_matches('/'), path)
        }
        None => webhook_url.to_string(),
    }
}

async fn relay_job(
    client: &Client,
    vault: &Arc<dyn WebhookVault>,
    state: &StateHandle,
    events: &tokio::sync::broadcast::Sender<AppEvent>,
    job: &RelayJob,
) {
    let entries = match vault.load() {
        Ok(entries) => entries,
        Err(e) => {
            tracing::debug!("discord vault load failed: {e}");
            return;
        }
    };
    // Re-validate at send time too: a vault implementation that skips
    // `parse_webhooks_json` must not aim photo bytes at an arbitrary
    // host either (defense in depth — the URL carries the token).
    let targets: Vec<&WebhookEntry> = entries
        .iter()
        .filter(|e| !job.auto_only || e.autopost)
        .filter(|e| validate_webhook_url(e.url.expose_secret()).is_ok())
        .take(MAX_WEBHOOKS)
        .collect();
    if targets.is_empty() {
        return;
    }
    // Read + validate the photo once for all targets.
    let Ok(meta) = std::fs::metadata(&job.path) else {
        return;
    };
    if !meta.is_file() || meta.len() > MAX_PHOTO_BYTES {
        return;
    }
    let Ok(bytes) = std::fs::read(&job.path) else {
        return;
    };
    let filename = job
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "photo.png".to_string());
    let mime = if filename.to_ascii_lowercase().ends_with(".jpg")
        || filename.to_ascii_lowercase().ends_with(".jpeg")
    {
        "image/jpeg"
    } else {
        "image/png"
    };
    let boundary = format!(
        "----VRCNextRust{}",
        OffsetDateTime::now_utc().unix_timestamp_nanos()
    );
    let body = build_multipart(&boundary, &filename, mime, &bytes);
    let content_type = format!("multipart/form-data; boundary={boundary}");

    let mut delivered = 0usize;
    let mut failed = 0usize;
    for (index, entry) in targets.iter().enumerate() {
        let url = entry.url.expose_secret().to_string();
        #[cfg(test)]
        let url = endpoint_for(&url, std::env::var("VRC_WEBHOOK_BASE").ok().as_deref());
        match client
            .post_external(&url, body.clone(), &content_type)
            .await
        {
            Ok(resp) if (200..300).contains(&resp.status) => delivered += 1,
            Ok(resp) => {
                failed += 1;
                // Status only — never the URL (it carries the token).
                tracing::debug!("discord webhook {index} rejected: HTTP {}", resp.status);
            }
            Err(e) => {
                failed += 1;
                // Kind only — `ApiError`'s Display can embed the webhook
                // URL (reqwest errors carry the request URL; `InvalidId`
                // stores it verbatim) and the URL holds the token.
                tracing::debug!("discord webhook {index} send failed: {}", error_kind(&e));
            }
        }
    }
    if failed > 0 && !job.auto_only {
        let message = format!("discord webhook: {delivered} sent, {failed} failed (see logs)");
        state.write().last_error = Some(message.clone());
        let _ = events.send(AppEvent::Error(message));
    }
}

/// Coarse, secret-free failure classification for logs. The full
/// [`vrc_api::ApiError`] is never logged on this path: its `Display`
/// can contain the webhook URL (RULES.md §4).
fn error_kind(e: &vrc_api::ApiError) -> &'static str {
    use vrc_api::ApiError as E;
    match e {
        E::Network(_) => "network error",
        E::Http { .. } => "http error",
        E::RateLimited { .. } => "rate limited",
        E::Unauthorized => "unauthorized",
        E::Requires2fa => "2fa required",
        E::Decode(_) => "decode error",
        E::Vault(_) => "vault error",
        E::InvalidId(_) => "invalid url",
        E::PayloadTooLarge { .. } => "response too large",
    }
}

fn build_multipart(boundary: &str, filename: &str, mime: &str, bytes: &[u8]) -> Vec<u8> {
    // Strip anything that could break out of the quoted header value.
    let filename = filename.replace(['"', '\r', '\n'], "_");
    let mut body = Vec::with_capacity(bytes.len() + 512);
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"files[0]\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {mime}\r\n\r\n").as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_url_validation() {
        assert!(validate_webhook_url(
            "https://discord.com/api/webhooks/123456789012345678/abcdefghijklmnopqrstuvwxyz"
        )
        .is_ok());
        assert!(validate_webhook_url("https://discordapp.com/api/webhooks/1/t").is_ok());
        assert!(validate_webhook_url("https://canary.discord.com/api/webhooks/1/t").is_ok());
        assert!(validate_webhook_url("http://discord.com/api/webhooks/1/t").is_err());
        assert!(validate_webhook_url("https://evil.com/api/webhooks/1/t").is_err());
        assert!(validate_webhook_url("https://discord.com/api/webhooks/1").is_err());
        assert!(validate_webhook_url("https://discord.com/api/other/1/t").is_err());
        assert!(validate_webhook_url("https://discord.com:8443/api/webhooks/1/t").is_err());
        assert!(validate_webhook_url("https://user@discord.com/api/webhooks/1/t").is_err());
    }

    #[tokio::test]
    async fn relay_posts_photo_to_mock_webhook() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/webhooks/1/token"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("shot.png");
        std::fs::write(&photo, b"not really a png").unwrap();

        let vault: Arc<dyn WebhookVault> = Arc::new(MemoryWebhookVault::with_entries(vec![
            WebhookEntry {
                url: SecretString::from("https://discord.com/api/webhooks/1/token".to_string()),
                autopost: true,
            },
            // auto_only jobs skip non-autopost webhooks.
            WebhookEntry {
                url: SecretString::from("https://discord.com/api/webhooks/2/token".to_string()),
                autopost: false,
            },
        ]));

        // endpoint_for rewrites the host to the wiremock server while
        // keeping the webhook path; the env var is read per send.
        std::env::set_var("VRC_WEBHOOK_BASE", server.uri());
        let client = Client::builder().rate_limit(1_000.0, 100).build().unwrap();
        let state = crate::app::StateHandle::detached();
        let (events, _rx) = tokio::sync::broadcast::channel(8);
        let job = RelayJob {
            path: photo,
            auto_only: true,
        };
        relay_job(&client, &vault, &state, &events, &job).await;
        std::env::remove_var("VRC_WEBHOOK_BASE");
        // `.expect(1)` on the mock verifies exactly one POST arrived —
        // the non-autopost webhook was skipped by `auto_only`.
    }

    #[test]
    fn vault_roundtrip_redacts_debug() {
        let vault = MemoryWebhookVault::new();
        let entries = vec![WebhookEntry {
            url: SecretString::from("https://discord.com/api/webhooks/1/t0ken".to_string()),
            autopost: true,
        }];
        vault.save(&entries).unwrap();
        let loaded = vault.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].autopost);
        assert_eq!(
            loaded[0].url.expose_secret(),
            entries[0].url.expose_secret()
        );
        let debug = format!("{:?}", loaded[0]);
        assert!(!debug.contains("t0ken"));
        assert!(debug.contains("redacted"));
    }

    #[test]
    fn slots_never_expose_url() {
        let entries = vec![WebhookEntry {
            url: SecretString::from("https://discord.com/api/webhooks/1/secret".to_string()),
            autopost: false,
        }];
        let slots = slots_for(&entries);
        assert_eq!(slots.len(), 1);
        assert!(!slots[0].autopost);
        assert!(!format!("{slots:?}").contains("secret"));
    }

    #[test]
    fn parse_json_caps_and_skips_bad_rows() {
        let json = r#"[
            {"url":"https://discord.com/api/webhooks/1/a","autopost":true},
            {"autopost":true},
            {"url":"https://discord.com/api/webhooks/2/b"},
            {"url":"https://evil.example.com/hook","autopost":true}
        ]"#;
        let entries = parse_webhooks_json(json).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].autopost);
        assert!(!entries[1].autopost);
    }

    #[test]
    fn endpoint_override_keeps_path() {
        let url = "https://discord.com/api/webhooks/1/token?wait=true";
        assert_eq!(
            endpoint_for(url, Some("http://127.0.0.1:1234/")),
            "http://127.0.0.1:1234/api/webhooks/1/token?wait=true"
        );
        assert_eq!(endpoint_for(url, None), url);
    }

    #[test]
    fn multipart_shape() {
        let body = build_multipart("b", "a.png", "image/png", &[1, 2, 3]);
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("--b\r\n"));
        assert!(text.contains("name=\"files[0]\"; filename=\"a.png\""));
        assert!(text.contains("Content-Type: image/png"));
        assert!(text.ends_with("--b--\r\n"));
    }
}
