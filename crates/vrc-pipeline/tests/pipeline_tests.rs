//! Pipeline integration tests against a local `tokio-tungstenite` server.
//! Synthetic frames only (RULES.md §6); no live network, no real tokens.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use secrecy::SecretString;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::http::header;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use vrc_api::USER_AGENT;
use vrc_pipeline::{Pipeline, PipelineConfig, PipelineEvent};

const FAKE_TOKEN: &str = "authcookie_00000000000000000000000000000000";
const USER_ID: &str = "usr_00000000-0000-0000-0000-000000000002";
const WAIT: Duration = Duration::from_secs(10);

fn friend_online_frame() -> String {
    format!(
        r#"{{"type":"friend-online","content":{{"userId":"{USER_ID}","user":{{"id":"{USER_ID}","displayName":"Test Friend","status":"join me","state":"online","tags":["system_trust_basic"],"location":"wrld_00000000-0000-0000-0000-000000000001:inst1"}}}}}}"#
    )
}

fn friend_offline_frame() -> String {
    format!(r#"{{"type":"friend-offline","content":{{"userId":"{USER_ID}","platform":"web"}}}}"#)
}

fn test_config(port: u16) -> PipelineConfig {
    PipelineConfig {
        url: format!("ws://127.0.0.1:{port}"),
        min_backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(300),
    }
}

/// Accept one WebSocket connection, asserting the handshake sends our
/// `User-Agent` and the auth cookie via `?authToken=` (RULES.md §2).
// The callback's `Err` type is fixed by `accept_hdr_async`.
#[allow(clippy::result_large_err)]
async fn accept_one(
    listener: &TcpListener,
    expected_ua_tx: mpsc::Sender<(String, String)>,
) -> WebSocketStream<TcpStream> {
    let (stream, _) = listener.accept().await.unwrap();
    let mut handshake = None;
    let ws = accept_hdr_async(stream, |req: &Request, resp: Response| {
        handshake = Some((
            req.headers()
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string(),
            req.uri().to_string(),
        ));
        Ok(resp)
    })
    .await
    .unwrap();
    expected_ua_tx.send(handshake.unwrap()).await.unwrap();
    ws
}

#[tokio::test]
async fn connects_and_decodes_frames_and_sends_user_agent() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (hs_tx, mut hs_rx) = mpsc::channel(4);

    let server: JoinHandle<()> = tokio::spawn(async move {
        let mut ws = accept_one(&listener, hs_tx).await;
        // Object-form content, a malformed frame (skipped), then string-form
        // content — the documented `content` quirk.
        ws.send(Message::Text(friend_online_frame().into()))
            .await
            .unwrap();
        ws.send(Message::Text("definitely not json".into()))
            .await
            .unwrap();
        ws.send(Message::Text(
            r#"{"type":"brand-new-kind","content":{"x":1}}"#.into(),
        ))
        .await
        .unwrap();
        // `friend-offline` with the content as a JSON-encoded *string*.
        let content_str =
            serde_json::to_string(&format!(r#"{{"userId":"{USER_ID}","platform":"web"}}"#))
                .unwrap();
        ws.send(Message::Text(
            format!(r#"{{"type":"friend-offline","content":{content_str}}}"#).into(),
        ))
        .await
        .unwrap();
        // Keep the socket open until the client finishes.
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let (handle, mut events) = Pipeline::connect(test_config(port), SecretString::from(FAKE_TOKEN));

    assert!(matches!(
        timeout(WAIT, events.next()).await.unwrap().unwrap(),
        PipelineEvent::Connected
    ));

    let (ua, uri) = hs_rx.recv().await.unwrap();
    assert_eq!(ua, USER_AGENT);
    assert!(uri.contains(&format!("authToken={FAKE_TOKEN}")));

    // friend-online (object form)
    let ev = timeout(WAIT, events.next()).await.unwrap().unwrap();
    match ev {
        PipelineEvent::FriendOnline { user_id, user } => {
            assert_eq!(user_id, USER_ID);
            assert_eq!(user.display_name, "Test Friend");
        }
        other => panic!("expected FriendOnline, got {other:?}"),
    }

    // malformed frame skipped → next is the unknown type
    let ev = timeout(WAIT, events.next()).await.unwrap().unwrap();
    match ev {
        PipelineEvent::Unknown { kind, .. } => assert_eq!(kind, "brand-new-kind"),
        other => panic!("expected Unknown, got {other:?}"),
    }

    // string-form content
    let ev = timeout(WAIT, events.next()).await.unwrap().unwrap();
    assert!(matches!(ev, PipelineEvent::FriendOffline { user_id, .. } if user_id == USER_ID));

    handle.shutdown();
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn reconnects_with_backoff_after_server_drop() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (hs_tx, _hs_rx) = mpsc::channel(4);

    // Accept the first connection, send a frame, then drop the socket. The
    // client must emit Disconnected, back off (~1 s, auto-advanced under the
    // paused clock), and reconnect to the same listener.
    let server = tokio::spawn(async move {
        {
            let mut ws = accept_one(&listener, hs_tx.clone()).await;
            ws.send(Message::Text(friend_online_frame().into()))
                .await
                .unwrap();
            ws.close(None).await.unwrap();
        }
        let mut ws = accept_one(&listener, hs_tx).await;
        ws.send(Message::Text(friend_offline_frame().into()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
    });

    let (handle, mut events) = Pipeline::connect(test_config(port), SecretString::from(FAKE_TOKEN));

    let mut saw_connected = 0u32;
    let mut saw_disconnected = false;
    let mut saw_offline = false;
    for _ in 0..8 {
        match timeout(WAIT, events.next()).await.unwrap().unwrap() {
            PipelineEvent::Connected => saw_connected += 1,
            PipelineEvent::Disconnected { .. } => saw_disconnected = true,
            PipelineEvent::FriendOffline { .. } => {
                saw_offline = true;
                break;
            }
            _ => {}
        }
    }
    handle.shutdown();
    server.abort();

    assert_eq!(saw_connected, 2, "expected an initial and a reconnect");
    assert!(saw_disconnected, "expected a Disconnected between them");
    assert!(saw_offline, "expected events after reconnect");
}

#[tokio::test]
async fn shutdown_ends_the_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (hs_tx, _hs_rx) = mpsc::channel(4);
    let server = tokio::spawn(async move {
        let _ws = accept_one(&listener, hs_tx).await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let (handle, mut events) = Pipeline::connect(test_config(port), SecretString::from(FAKE_TOKEN));
    assert!(matches!(
        timeout(WAIT, events.next()).await.unwrap().unwrap(),
        PipelineEvent::Connected
    ));
    handle.join().await;
    assert!(timeout(WAIT, events.next()).await.unwrap().is_none());
    server.abort();
}

#[test]
fn secrets_never_appear_in_debug() {
    let config = test_config(1);
    let debug = format!("{config:?}");
    assert!(!debug.contains(FAKE_TOKEN));
    assert!(!debug.contains("authToken"));
}

#[tokio::test]
async fn server_close_reason_is_sanitized() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (hs_tx, _hs_rx) = mpsc::channel(4);
    // Close with a reason echoing the token: it must be scrubbed before the
    // Disconnected reason reaches subscribers (RULES.md §4).
    let server = tokio::spawn(async move {
        let mut ws = accept_one(&listener, hs_tx).await;
        ws.close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
            code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal,
            reason: format!("banned authToken={FAKE_TOKEN}").into(),
        }))
        .await
        .unwrap();
    });

    let (handle, mut events) = Pipeline::connect(test_config(port), SecretString::from(FAKE_TOKEN));
    assert!(matches!(
        timeout(WAIT, events.next()).await.unwrap().unwrap(),
        PipelineEvent::Connected
    ));
    let mut reason = String::new();
    for _ in 0..4 {
        if let PipelineEvent::Disconnected { reason: r } =
            timeout(WAIT, events.next()).await.unwrap().unwrap()
        {
            reason = r;
            break;
        }
    }
    handle.shutdown();
    server.abort();
    assert!(!reason.is_empty(), "expected a Disconnected event");
    assert!(
        reason.contains("[redacted]"),
        "reason not sanitized: {reason}"
    );
    assert!(!reason.contains(FAKE_TOKEN), "token leaked: {reason}");
}
