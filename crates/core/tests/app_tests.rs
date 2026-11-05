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
    AuthState, Client, Command, MemoryVault, PipelineConfig, PipelineEvent, PipelineStatus,
    TwoFactorMethod,
};

const CURRENT_USER: &str = include_str!("../../vrc-api/tests/fixtures/current_user.json");
const FAKE_COOKIE: &str = "authcookie_00000000000000000000000000000000";
const ONLINE_ID: &str = "usr_00000000-0000-0000-0000-000000000002";
const ACTIVE_ID: &str = "usr_00000000-0000-0000-0000-000000000003";
const OFFLINE_ID: &str = "usr_00000000-0000-0000-0000-000000000004";
const FAV_ID: &str = "usr_00000000-0000-0000-0000-000000000005";
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
        .and(path("/auth/user/friends"))
        .and(query_param("offline", "false"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![
            fake_friend(
                ONLINE_ID,
                "Test Online",
                "wrld_00000000-0000-0000-0000-00000000000a:inst1",
            ),
            fake_friend(ACTIVE_ID, "Test Active", ""),
            fake_friend(FAV_ID, "Test Favorite", ""),
        ]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth/user/friends"))
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
            "favoriteId": FAV_ID,
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
    assert_eq!(state.friends.len(), 4);
    let s = sections(&state);
    assert_eq!(s.in_game.len(), 1);
    assert_eq!(s.in_game[0].id, ONLINE_ID);
    // "Test Active" (no location) lands in Web/Active.
    assert_eq!(s.web_active.len(), 1);
    assert_eq!(s.web_active[0].id, ACTIVE_ID);
    assert_eq!(s.offline.len(), 1);
    assert_eq!(s.offline[0].id, OFFLINE_ID);
    // Sections are disjoint: the online favorite appears only in `favorites`.
    assert_eq!(s.favorites.len(), 1);
    assert_eq!(s.favorites[0].id, FAV_ID);

    // Case-insensitive live search keeps the section grouping.
    let hits = search(&s, "test on");
    assert_eq!(hits.in_game.len(), 1);
    assert_eq!(hits.in_game[0].id, ONLINE_ID);
    let hits = search(&s, "TEST OFFLINE");
    assert_eq!(hits.offline[0].id, OFFLINE_ID);

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
    assert!(!state.friends[ONLINE_ID].is_online());
    assert_eq!(state.friends[ONLINE_ID].state, "offline");
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

    // friend-online for a favorite keeps it in `favorites`, never in_game,
    // and puts it on the worlds dashboard.
    frames_tx
        .send(format!(
            r#"{{"type":"friend-online","content":{{"userId":"{FAV_ID}","user":{}}}}}"#,
            pipeline_user(
                FAV_ID,
                "Test Favorite",
                "wrld_00000000-0000-0000-0000-00000000000c:inst7",
                "online"
            )
        ))
        .await
        .unwrap();
    wait_for(&mut handle, |e| {
        matches!(e, AppEvent::Pipeline(ev) if matches!(&**ev, PipelineEvent::FriendOnline { .. }))
    })
    .await;
    let state = handle.state.snapshot();
    let s = sections(&state);
    assert!(s.favorites.iter().any(|f| f.id == FAV_ID));
    assert!(s.in_game.iter().all(|f| f.id != FAV_ID));
    let groups = worlds_with_friends(&state);
    assert!(groups
        .iter()
        .any(|g| g.friends.iter().any(|f| f.id == FAV_ID)));

    // friend-add inserts a brand-new friend; friend-delete removes it —
    // still no REST round trip.
    const NEW_ID: &str = "usr_00000000-0000-0000-0000-000000000006";
    frames_tx
        .send(format!(
            r#"{{"type":"friend-add","content":{{"userId":"{NEW_ID}","user":{}}}}}"#,
            pipeline_user(NEW_ID, "Test Newcomer", "", "active")
        ))
        .await
        .unwrap();
    wait_for(
        &mut handle,
        |e| matches!(e, AppEvent::Pipeline(ev) if matches!(&**ev, PipelineEvent::FriendAdd { .. })),
    )
    .await;
    let state = handle.state.snapshot();
    assert_eq!(state.friends[NEW_ID].display_name, "Test Newcomer");
    assert!(sections(&state).web_active.iter().any(|f| f.id == NEW_ID));

    frames_tx
        .send(format!(
            r#"{{"type":"friend-delete","content":{{"userId":"{NEW_ID}"}}}}"#
        ))
        .await
        .unwrap();
    wait_for(&mut handle, |e| {
        matches!(e, AppEvent::Pipeline(ev) if matches!(&**ev, PipelineEvent::FriendDelete { .. }))
    })
    .await;
    assert!(!handle.state.snapshot().friends.contains_key(NEW_ID));

    // The event stream is live state — no extra REST calls happened.
    server.verify().await;
}

#[tokio::test]
async fn login_needs_2fa_then_submit_2fa_authenticates() {
    let server = MockServer::start().await;
    // First login attempt: credentials accepted but a second factor is due.
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "requiresTwoFactorAuth": ["totp", "emailOtp"]
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // After verification the same endpoint returns the full user + cookie.
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(CURRENT_USER, "application/json")
                .insert_header("set-cookie", format!("auth={FAKE_COOKIE}; Path=/")),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/twofactorauth/totp/verify"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "verified": true })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth/user/friends"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<serde_json::Value>::new()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/favorites"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<serde_json::Value>::new()))
        .mount(&server)
        .await;

    // A local socket to hold the post-login pipeline connection.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let _ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
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
    let ev = wait_for(&mut handle, |e| {
        matches!(e, AppEvent::AuthChanged(AuthState::Needs2fa(_)))
    })
    .await;
    let AppEvent::AuthChanged(AuthState::Needs2fa(methods)) = ev else {
        unreachable!()
    };
    assert!(methods.contains(&TwoFactorMethod::Totp));

    handle
        .commands
        .send(Command::Submit2fa {
            method: TwoFactorMethod::Totp,
            code: SecretString::from("000000"),
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| {
        matches!(e, AppEvent::AuthChanged(AuthState::Authenticated))
    })
    .await;
    wait_for(&mut handle, |e| matches!(e, AppEvent::FriendsRefreshed)).await;

    let state = handle.state.snapshot();
    assert_eq!(state.auth, AuthState::Authenticated);
    assert!(state.current_user.is_some());
    server.verify().await;
}

#[tokio::test]
async fn unauthorized_on_refresh_tears_down_session() {
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
    // The session died server-side: the post-login friends fetch is rejected.
    Mock::given(method("GET"))
        .and(path("/auth/user/friends"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let _ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
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
    // The 401 during the login-time friends refresh tears the session down.
    wait_for(&mut handle, |e| {
        matches!(e, AppEvent::AuthChanged(AuthState::LoggedOut))
    })
    .await;

    let state = handle.state.snapshot();
    assert_eq!(state.auth, AuthState::LoggedOut);
    assert!(state.current_user.is_none());
    assert!(state.friends.is_empty());
    assert_eq!(state.pipeline, PipelineStatus::Disconnected);
}
