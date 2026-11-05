//! Reconnecting WebSocket connection to the VRChat pipeline.
//!
//! [`Pipeline::connect`] spawns a task that keeps a `?authToken=` WebSocket
//! open, decodes each text frame through [`crate::events::decode`], and
//! reconnects with capped exponential backoff + jitter (1 s → 5 min) after
//! any drop (RULES.md §3). The auth token is appended to the URI in memory
//! only — it is never logged and `Debug` impls never expose it (RULES.md §4).

use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::{Stream, StreamExt};
use secrecy::{ExposeSecret, SecretString};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use vrc_api::USER_AGENT;

use crate::events::{decode, PipelineEvent};

/// Documented pipeline endpoint.
pub const PIPELINE_URL: &str = "wss://pipeline.vrchat.cloud";

/// Upper bound for a single WebSocket handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Static connection tuning for [`Pipeline::connect`].
///
/// `url` may be overridden to point at a local test server; it never contains
/// the auth token (the token is appended as `?authToken=` at handshake time
/// and never logged).
#[derive(Clone)]
pub struct PipelineConfig {
    /// Pipeline endpoint (`ws://` or `wss://`). Default: [`PIPELINE_URL`].
    pub url: String,
    /// First reconnect delay; doubles per attempt. Default: 1 s.
    pub min_backoff: Duration,
    /// Reconnect delay ceiling. Default: 5 min.
    pub max_backoff: Duration,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            url: PIPELINE_URL.to_string(),
            min_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(300),
        }
    }
}

impl fmt::Debug for PipelineConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PipelineConfig")
            .field("min_backoff", &self.min_backoff)
            .field("max_backoff", &self.max_backoff)
            .finish_non_exhaustive()
    }
}

/// Handle controlling a running pipeline connection.
///
/// Dropping the handle also signals shutdown.
pub struct PipelineHandle {
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl PipelineHandle {
    /// Signal the connection task to stop; the event stream then ends.
    /// Idempotent.
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    /// Signal shutdown and wait for the connection task to finish.
    pub async fn join(mut self) {
        self.shutdown();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for PipelineHandle {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

impl fmt::Debug for PipelineHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PipelineHandle(..)")
    }
}

/// Stream end of the pipeline connection: yields [`PipelineEvent`]s until
/// shutdown or the connection task ends.
///
/// Implements [`Stream`]; [`PipelineEvents::recv`] is a small convenience
/// that avoids needing `StreamExt` for the common "await next event" loop.
pub struct PipelineEvents {
    rx: mpsc::Receiver<PipelineEvent>,
}

impl PipelineEvents {
    /// Await the next event; `None` once the pipeline is shut down for good.
    pub async fn recv(&mut self) -> Option<PipelineEvent> {
        self.rx.recv().await
    }
}

impl Stream for PipelineEvents {
    type Item = PipelineEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<PipelineEvent>> {
        self.rx.poll_recv(cx)
    }
}

impl fmt::Debug for PipelineEvents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PipelineEvents(..)")
    }
}

/// The reconnecting pipeline connection.
pub struct Pipeline;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

impl Pipeline {
    /// Connect to the pipeline, returning a control handle and the event
    /// stream. Must be called from within a Tokio runtime.
    ///
    /// The connection task runs until [`PipelineHandle::shutdown`] (or the
    /// handle is dropped). `Connected`/`Disconnected` bracket each session;
    /// reconnects are automatic with capped exponential backoff + jitter.
    pub fn connect(
        config: PipelineConfig,
        auth_token: SecretString,
    ) -> (PipelineHandle, impl Stream<Item = PipelineEvent> + Send) {
        let (tx, rx) = mpsc::channel(256);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(connect_loop(config, auth_token, tx, shutdown_rx));
        (
            PipelineHandle {
                shutdown: shutdown_tx,
                task: Some(task),
            },
            PipelineEvents { rx },
        )
    }
}

async fn connect_loop(
    config: PipelineConfig,
    token: SecretString,
    tx: mpsc::Sender<PipelineEvent>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut attempt = 0u32;
    let mut rng = rng_seed();
    loop {
        let connected = tokio::select! {
            _ = shutdown.changed() => return,
            result = timeout(CONNECT_TIMEOUT, handshake(&config.url, &token)) => match result {
                Ok(Ok(stream)) => Some(stream),
                Ok(Err(e)) => {
                    // Error text could echo the request URI (which contains
                    // the token) — strip it before logging (RULES.md §4).
                    let message = sanitize(&e.to_string(), &token);
                    tracing::debug!("pipeline: handshake failed: {message}");
                    let _ = tx
                        .send(PipelineEvent::Disconnected {
                            reason: "handshake failed".to_string(),
                        })
                        .await;
                    None
                }
                Err(_) => {
                    tracing::debug!("pipeline: connect timed out");
                    let _ = tx
                        .send(PipelineEvent::Disconnected {
                            reason: "connect timed out".to_string(),
                        })
                        .await;
                    None
                }
            }
        };

        if let Some(mut ws) = connected {
            attempt = 0;
            tracing::debug!("pipeline: connected");
            let _ = tx.send(PipelineEvent::Connected).await;
            match read_loop(&mut ws, &tx, &mut shutdown).await {
                ReadEnd::Shutdown => return,
                ReadEnd::Dropped(reason) => {
                    // Server close reasons and tungstenite errors can echo
                    // the request URI — strip the token before the reason
                    // reaches logs or subscribers (RULES.md §4).
                    let reason = sanitize(&reason, &token);
                    tracing::debug!("pipeline: disconnected ({reason})");
                    let _ = tx.send(PipelineEvent::Disconnected { reason }).await;
                }
            }
        }

        let delay = backoff(attempt, &mut rng, &config);
        attempt = attempt.saturating_add(1);
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = sleep(delay) => {}
        }
    }
}

/// How a connected session ended.
enum ReadEnd {
    /// `shutdown()` was requested.
    Shutdown,
    /// The server closed the socket or the transport errored.
    Dropped(String),
}

async fn read_loop(
    ws: &mut WsStream,
    tx: &mpsc::Sender<PipelineEvent>,
    shutdown: &mut watch::Receiver<bool>,
) -> ReadEnd {
    loop {
        tokio::select! {
            _ = shutdown.changed() => return ReadEnd::Shutdown,
            msg = ws.next() => match msg {
                None => return ReadEnd::Dropped("connection closed".to_string()),
                Some(Err(e)) => {
                    return ReadEnd::Dropped(format!("websocket error: {e}"));
                }
                Some(Ok(Message::Close(frame))) => {
                    let reason = frame
                        .map(|f| format!("closed by server: {}", f.reason))
                        .unwrap_or_else(|| "closed by server".to_string());
                    return ReadEnd::Dropped(reason);
                }
                Some(Ok(Message::Text(text))) => {
                    if let Some(event) = decode(&text) {
                        if tx.send(event).await.is_err() {
                            return ReadEnd::Shutdown;
                        }
                    }
                }
                // Pings are answered by tungstenite; binary/pong frames carry
                // no events.
                Some(Ok(_)) => {}
            },
        }
    }
}

/// Build the handshake request: `GET url/?authToken=<cookie>` with the
/// mandatory `User-Agent` (RULES.md §2). The token only lives inside this
/// function's request — callers must never log it (RULES.md §4).
async fn handshake(
    url: &str,
    token: &SecretString,
) -> Result<WsStream, tokio_tungstenite::tungstenite::Error> {
    let uri = format!(
        "{}/?authToken={}",
        url.trim_end_matches('/'),
        percent_encode(token.expose_secret())
    );
    let mut request = uri.into_client_request()?;
    request
        .headers_mut()
        .insert(header::USER_AGENT, HeaderValue::from_static(USER_AGENT));
    let (stream, _response) = connect_async(request).await?;
    Ok(stream)
}

/// Exponential backoff `min << attempt` capped at `max`, plus up to 500 ms of
/// jitter so reconnecting clients do not converge (RULES.md §3).
fn backoff(attempt: u32, rng: &mut u64, config: &PipelineConfig) -> Duration {
    let base = config.min_backoff.saturating_mul(1u32 << attempt.min(9));
    let jitter = Duration::from_millis(next_u64(rng) % 500);
    base.saturating_add(jitter).min(config.max_backoff)
}

/// Replace the secret wherever it may appear in `msg` — raw *and* in the
/// percent-encoded form the request URI carries (RULES.md §4). An empty
/// token matches everywhere, so the message passes through unchanged.
fn sanitize(msg: &str, token: &SecretString) -> String {
    let raw = token.expose_secret();
    if raw.is_empty() {
        return msg.to_string();
    }
    msg.replace(raw, "[redacted]")
        .replace(&percent_encode(raw), "[redacted]")
}

/// Percent-encode for a URI query value (unreserved chars pass through).
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    out
}

fn rng_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(0x9e37_79b9_7f4a_7c15)
        | 1
}

/// xorshift64* — decorrelates reconnects without a `rand` dependency.
fn next_u64(rng: &mut u64) -> u64 {
    let mut x = *rng;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *rng = x;
    x.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_from_min_and_caps_at_max() {
        let config = PipelineConfig::default();
        let mut rng = 1u64;
        let first = backoff(0, &mut rng, &config);
        let later = backoff(5, &mut rng, &config);
        assert!(first >= Duration::from_secs(1) && first < Duration::from_secs(2));
        assert!(later >= Duration::from_secs(32));
        assert_eq!(backoff(20, &mut rng, &config), Duration::from_secs(300));
    }

    #[test]
    fn default_config_points_at_documented_endpoint() {
        let config = PipelineConfig::default();
        assert_eq!(config.url, PIPELINE_URL);
        assert_eq!(config.min_backoff, Duration::from_secs(1));
        assert_eq!(config.max_backoff, Duration::from_secs(300));
        assert!(!format!("{config:?}").contains("pipeline.vrchat"));
    }

    #[test]
    fn tokens_are_percent_encoded_and_sanitized_from_messages() {
        assert_eq!(percent_encode("a&b=c"), "a%26b%3Dc");
        let token = SecretString::from("authcookie_secret");
        assert_eq!(
            sanitize("err wss://x/?authToken=authcookie_secret&y=1", &token),
            "err wss://x/?authToken=[redacted]&y=1"
        );
        // The encoded form found in the request URI is scrubbed too.
        let token = SecretString::from("tok&en");
        assert_eq!(
            sanitize("err wss://x/?authToken=tok%26en", &token),
            "err wss://x/?authToken=[redacted]"
        );
    }

    #[test]
    fn sanitize_leaves_message_alone_when_token_is_empty() {
        let msg = "closed by server: going away";
        assert_eq!(sanitize(msg, &SecretString::from("")), msg);
    }
}
