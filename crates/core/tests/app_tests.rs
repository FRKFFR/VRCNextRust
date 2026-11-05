//! Headless `vrc-core` integration test: wiremock REST + a local
//! `tokio-tungstenite` server, synthetic fixtures only (RULES.md §6).

use std::time::Duration;

use futures_util::SinkExt;
use secrecy::SecretString;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use vrc_core::{
    search, sections, worlds_with_friends, App, AppConfig, AppEvent, AppHandle, AppState,
    AuthState, Client, Command, MemoryVault, PipelineConfig, PipelineEvent,
};

const CURRENT_USER: &str = include_str!("../../vrc-api/tests/fixtures/current_user.json");
const FAKE_COOKIE: &str = "authcookie_00000000000000000000000000000000";
const ONLINE_ID: &str = "usr_00000000-0000-0000-0000-000000000002";
const ACTIVE_ID: &str = "usr_00000000-0000-0000-0000-000000000003";
const OFFLINE_ID: &str = "usr_00000000-0000-0000-0000-000000000004";
const WAIT: Duration = Duration::from_secs(15);

fn fake_friend(id: &str, name: &str, location: &str) -> serde_json::Value {
    json!({
        "id": id,
        "displayName": name,
        "status": "active",
        "statusDescription": "synthetic",
        "state": "",
        "location": location,
        "last_platform": "standalonewindows",
        "last_activity": "2026-01-02T03:04:05.000Z",
        "tags": ["system_trust_basic", "system_trust_known", "language_eng"],
        "userIcon": "",
        "bio": "Synthetic test fixture.",
        "bioLinks": [],
        "friendKey": "friendkey_synthetic",
        "isFriend": true,
        "currentAvatarThumbnailImageUrl": "",
        "profilePicOverride": "",
        "developerType": "none"
    })
}

/// Synthetic pipeline user object embedded in friend-* event content.
fn pipeline_user(id: &str, name: &str, location: &str, state: &str) -> serde_json::Value {
    json!({
        "id": id,
        "displayName": name,
        "status": "join me",
        "statusDescription": "synthetic",
        "state": state,
        "location": location,
        "tags": ["system_trust_basic", "system_trust_known"],
        "last_platform": "standalonewindows"
    })
}

async fn next_event(handle: &mut AppHandle) -> AppEvent {
    loop {
        match timeout(WAIT, handle.events.recv()).await {
            Ok(Ok(event)) => return event,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            other => panic!("event stream ended: {other:?}"),
        }
    }
}

async fn wait_for(handle: &mut AppHandle, wanted: impl Fn(&AppEvent) -> bool) -> AppEvent {
    loop {
        let event = next_event(handle).await;
        if wanted(&event) {
            return event;
        }
    }
}

#[tokio::test]
async fn login_fetches_friends_once_and_events_update_state_incrementally() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(CURRENT_USER, "application/json")
                .insert_header("set-cookie", format!("auth={FAKE_COOKIE}; Path=/")),
        )
        .mount(&server)
        .await;
    // Each list endpoint must be hit exactly once per refresh (RULES.md §3).
    Mock::given(method("GET"))
        .and(path("/friends"))
        .and(query_param("offline", "false"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![
            fake_friend(
                ONLINE_ID,
                "Test Online",
                "wrld_00000000-0000-0000-0000-00000000000a:inst1",
            ),
            fake_friend(ACTIVE_ID, "Test Active", ""),
        ]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/friends"))
        .and(query_param("offline", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![fake_friend(
            OFFLINE_ID,
            "Test Offline",
            "offline",
        )]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/favorites"))
        .and(query_param("type", "friend"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![json!({
            "id": "fv_00000000-0000-0000-0000-000000000001",
            "type": "friend",
            "favoriteId": ONLINE_ID,
            "tags": [],
            "visibility": "friends"
        })]))
        .expect(1)
        .mount(&server)
        .await;

    // Local WebSocket "pipeline" the test can push frames through.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (frames_tx, mut frames_rx) = mpsc::channel::<String>(16);
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(text) = frames_rx.recv().await {
            ws.send(Message::Text(text.into())).await.unwrap();
        }
    });

    let client = Client::builder()
        .base_url(server.uri())
        .vault(MemoryVault::new())
        .rate_limit(1_000.0, 100)
        .build()
        .unwrap();
    let config = AppConfig {
        client,
        pipeline: PipelineConfig {
            url: format!("ws://127.0.0.1:{port}"),
            ..PipelineConfig::default()
        },
    };
    let mut handle = App::spawn(config);

    handle
        .commands
        .send(Command::Login {
            username: "testuser".to_string(),
            password: SecretString::from("synthetic password"),
        })
        .await
        .unwrap();

    wait_for(&mut handle, |e| {
        matches!(e, AppEvent::AuthChanged(AuthState::Authenticated))
    })
    .await;
    wait_for(&mut handle, |e| matches!(e, AppEvent::FriendsRefreshed)).await;
    wait_for(
        &mut handle,
        |e| matches!(e, AppEvent::Pipeline(ev) if matches!(&**ev, PipelineEvent::Connected)),
    )
    .await;

    let state: AppState = handle.state.snapshot();
    assert_eq!(state.friends.len(), 3);
    let s = sections(&state);
    assert_eq!(s.in_game.len(), 1);
    assert_eq!(s.active.len(), 1);
    assert_eq!(s.offline.len(), 1);
    assert_eq!(s.favorites.len(), 1);
    assert_eq!(s.favorites[0].id, ONLINE_ID);

    // Case-insensitive live search over the loaded list.
    assert_eq!(search(&state, "test on").len(), 1);
    assert_eq!(search(&state, "TEST OFFLINE")[0].id, OFFLINE_ID);

    // friend-offline moves the friend between sections — no re-fetch.
    frames_tx
        .send(format!(
            r#"{{"type":"friend-offline","content":{{"userId":"{ONLINE_ID}","platform":"standalonewindows"}}}}"#
        ))
        .await
        .unwrap();
    wait_for(&mut handle, |e| {
        matches!(e, AppEvent::Pipeline(ev) if matches!(&**ev, PipelineEvent::FriendOffline { .. }))
    })
    .await;
    let state = handle.state.snapshot();
    assert!(!state.friends[ONLINE_ID].is_online);
    let s = sections(&state);
    assert_eq!(s.in_game.len(), 0);
    assert_eq!(s.offline.len(), 2);

    // friend-location moves the web-active friend in-game and onto the
    // worlds dashboard.
    frames_tx
        .send(format!(
            r#"{{"type":"friend-location","content":{{"userId":"{ACTIVE_ID}","location":"wrld_00000000-0000-0000-0000-00000000000b:inst9","worldId":"wrld_00000000-0000-0000-0000-00000000000b","user":{}}}}}"#,
            pipeline_user(
                ACTIVE_ID,
                "Test Active",
                "wrld_00000000-0000-0000-0000-00000000000b:inst9",
                "online"
            )
        ))
        .await
        .unwrap();
    wait_for(&mut handle, |e| {
        matches!(e, AppEvent::Pipeline(ev) if matches!(&**ev, PipelineEvent::FriendLocation { .. }))
    })
    .await;
    let state = handle.state.snapshot();
    let s = sections(&state);
    assert_eq!(s.in_game.len(), 1);
    assert_eq!(s.in_game[0].id, ACTIVE_ID);
    let groups = worlds_with_friends(&state);
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0].world_id,
        "wrld_00000000-0000-0000-0000-00000000000b"
    );
    assert_eq!(groups[0].friends.len(), 1);

    // The event stream is live state — no extra REST calls happened.
    server.verify().await;
}
