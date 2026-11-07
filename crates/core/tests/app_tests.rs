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
    dashboard, search, sections, worlds_with_friends, App, AppConfig, AppEvent, AppHandle,
    AppState, AuthState, Client, Command, FriendHistoryFilter, MemoryVault, NewSession,
    PipelineConfig, PipelineEvent, PipelineStatus, PlayerModerationKind, StatsRange,
    TimelineFilter, TimelineKind, TwoFactorMethod, WorldName,
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
        data_dir: None,
        // Headless tests never touch the real log dir or data dir.
        log_dir: Some(std::path::PathBuf::from("vrc-core-test-no-such-log-dir")),
        store: Some(vrc_core::Store::open_in_memory().unwrap()),
        local_writes: true,
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
        data_dir: None,
        // Headless tests never touch the real log dir or data dir.
        log_dir: Some(std::path::PathBuf::from("vrc-core-test-no-such-log-dir")),
        store: Some(vrc_core::Store::open_in_memory().unwrap()),
        local_writes: true,
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
        data_dir: None,
        // Headless tests never touch the real log dir or data dir.
        log_dir: Some(std::path::PathBuf::from("vrc-core-test-no-such-log-dir")),
        store: Some(vrc_core::Store::open_in_memory().unwrap()),
        local_writes: true,
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
    assert!(state.world_names.is_empty());
}

#[tokio::test]
async fn dashboard_world_names_resolve_once_and_failures_are_cached() {
    const WORLD_OK: &str = "wrld_00000000-0000-0000-0000-0000000000a1";
    const WORLD_BAD: &str = "wrld_00000000-0000-0000-0000-0000000000b2";
    const FRIEND_A: &str = "usr_00000000-0000-0000-0000-0000000000a1";
    const FRIEND_B: &str = "usr_00000000-0000-0000-0000-0000000000b2";

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
    Mock::given(method("GET"))
        .and(path("/auth/user/friends"))
        .and(query_param("offline", "false"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![
            fake_friend(FRIEND_A, "Test In World", &format!("{WORLD_OK}:inst1")),
            fake_friend(FRIEND_B, "Test Gone World", &format!("{WORLD_BAD}:inst2")),
        ]))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth/user/friends"))
        .and(query_param("offline", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<serde_json::Value>::new()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/favorites"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<serde_json::Value>::new()))
        .mount(&server)
        .await;
    // One world resolves, one is gone — both looked up exactly once.
    Mock::given(method("GET"))
        .and(path(format!("/worlds/{WORLD_OK}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": WORLD_OK,
            "name": "Test World Name"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/worlds/{WORLD_BAD}")))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
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
        data_dir: None,
        // Headless tests never touch the real log dir or data dir.
        log_dir: Some(std::path::PathBuf::from("vrc-core-test-no-such-log-dir")),
        store: Some(vrc_core::Store::open_in_memory().unwrap()),
        local_writes: true,
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

    // Both resolutions report back; order between them is not guaranteed.
    let mut resolved = std::collections::HashSet::new();
    for _ in 0..2 {
        match wait_for(&mut handle, |e| {
            matches!(e, AppEvent::WorldNameResolved { .. })
        })
        .await
        {
            AppEvent::WorldNameResolved { world_id } => {
                resolved.insert(world_id);
            }
            _ => unreachable!(),
        }
    }
    assert!(resolved.contains(WORLD_OK));
    assert!(resolved.contains(WORLD_BAD));

    let state = handle.state.snapshot();
    assert_eq!(
        state.world_names.get(WORLD_OK),
        Some(&WorldName::Known("Test World Name".to_string()))
    );
    assert!(matches!(
        state.world_names.get(WORLD_BAD),
        Some(WorldName::Unknown { .. })
    ));

    // The dashboard surfaces the resolved name and keeps friend-count order.
    let entries = dashboard(&state);
    assert_eq!(entries.len(), 2);
    let named = entries
        .iter()
        .find(|e| e.world_id == WORLD_OK)
        .expect("dashboard entry for resolved world");
    assert_eq!(named.world_name.as_deref(), Some("Test World Name"));
    assert_eq!(vrc_core::world_title(named), "Test World Name");

    server.verify().await;
}

/// Poll `cond` until it holds or ~10s elapse.
async fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// A headless app with an in-memory store and a (possibly empty) log dir.
fn headless_config(log_dir: std::path::PathBuf) -> (AppConfig, vrc_core::Store) {
    let store = vrc_core::Store::open_in_memory().unwrap();
    let config = AppConfig {
        client: Client::builder()
            .base_url("http://127.0.0.1:1")
            .vault(MemoryVault::new())
            .rate_limit(1_000.0, 100)
            .build()
            .unwrap(),
        pipeline: PipelineConfig::default(),
        data_dir: None,
        log_dir: Some(log_dir),
        store: Some(store.clone()),
        local_writes: true,
    };
    (config, store)
}

#[tokio::test]
async fn log_tail_persists_timeline_sessions_and_stats() {
    let dir = tempfile::tempdir().unwrap();
    let (config, store) = headless_config(dir.path().to_path_buf());
    let mut handle = App::spawn(config);
    // `log_tail_active` is set once the core thread finishes startup.
    assert!(wait_until(|| handle.state.snapshot().log_tail_active).await);

    // Let the tailer's first directory scan complete so the file we write
    // next counts as "new" (read from the start, not tailed-from-end).
    tokio::time::sleep(Duration::from_millis(800)).await;
    std::fs::write(
        dir.path().join("output_log_2024-05-14_20-45-00.txt"),
        concat!(
            "2024.05.14 20:45:00 Log - [Behaviour] Joining wrld_00000000-0000-0000-0000-00000000000a:1234~region(us)\n",
            "2024.05.14 20:45:01 Log - [Behaviour] Entering Room: Test World\n",
            "2024.05.14 20:45:10 Log - [Behaviour] OnPlayerJoined Test Friend (usr_00000000-0000-0000-0000-000000000003)\n",
            "2024.05.14 20:45:40 Log - [VRC Camera] Took screenshot to: C:\\fake\\pic.png\n",
            "2024.05.14 20:46:00 Log - [Behaviour] OnLeftRoom\n",
        ),
    )
    .unwrap();

    // The store is shared with the test: wait until all five events landed.
    assert!(
        wait_until(|| {
            store
                .timeline()
                .count(&TimelineFilter::default())
                .unwrap_or(0)
                == 5
        })
        .await,
        "timeline rows never arrived"
    );

    // Commands surface the same rows through AppState.
    handle
        .commands
        .send(Command::LoadTimeline {
            page: 0,
            filter: TimelineFilter::default(),
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::TimelineLoaded)).await;
    let page = handle.state.snapshot().timeline.unwrap();
    assert_eq!(page.total, 5);
    assert_eq!(page.entries.len(), 5);
    // Newest first: LeftWorld is the last log line.
    assert_eq!(page.entries[0].kind, TimelineKind::LeftWorld);
    assert!(page
        .entries
        .iter()
        .any(|e| e.kind == TimelineKind::PhotoTaken && e.detail.contains("pic.png")));

    // Kind filter narrows the page.
    handle
        .commands
        .send(Command::LoadTimeline {
            page: 0,
            filter: TimelineFilter {
                kind: Some(TimelineKind::PlayerJoined),
                ..TimelineFilter::default()
            },
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::TimelineLoaded)).await;
    let page = handle.state.snapshot().timeline.unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.entries[0].display_name, "Test Friend");
    assert_eq!(
        page.entries[0].user_id,
        "usr_00000000-0000-0000-0000-000000000003"
    );

    // The Joining/Entering pair produced ONE 60-second session named
    // "Test World"; LeftWorld closed it.
    handle
        .commands
        .send(Command::LoadStats {
            range: StatsRange::default(),
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::StatsLoaded)).await;
    let stats = handle.state.snapshot().stats.unwrap();
    assert_eq!(stats.session_count, 1);
    assert_eq!(stats.total_seconds, 60);
    assert_eq!(stats.top_worlds.len(), 1);
    assert_eq!(
        stats.top_worlds[0].world_id,
        "wrld_00000000-0000-0000-0000-00000000000a"
    );
    assert_eq!(stats.top_worlds[0].world_name, "Test World");
    assert!(store.sessions().current().unwrap().is_none());
}

#[tokio::test]
async fn pipeline_events_persist_friend_history() {
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

    let store = vrc_core::Store::open_in_memory().unwrap();
    let store_for_wait = store.clone();
    let config = AppConfig {
        client: Client::builder()
            .base_url(server.uri())
            .vault(MemoryVault::new())
            .rate_limit(1_000.0, 100)
            .build()
            .unwrap(),
        pipeline: PipelineConfig {
            url: format!("ws://127.0.0.1:{port}"),
            ..PipelineConfig::default()
        },
        data_dir: None,
        log_dir: Some(std::path::PathBuf::from("vrc-core-test-no-such-log-dir")),
        store: Some(store),
        local_writes: true,
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

    frames_tx
        .send(format!(
            r#"{{"type":"friend-location","content":{{"userId":"{ONLINE_ID}","location":"wrld_00000000-0000-0000-0000-00000000000d:55~region(us)","worldId":"wrld_00000000-0000-0000-0000-00000000000d","user":{}}}}}"#,
            pipeline_user(
                ONLINE_ID,
                "Test Online",
                "wrld_00000000-0000-0000-0000-00000000000d:55~region(us)",
                "online"
            )
        ))
        .await
        .unwrap();
    wait_for(&mut handle, |e| {
        matches!(e, AppEvent::Pipeline(ev) if matches!(&**ev, PipelineEvent::FriendLocation { .. }))
    })
    .await;

    // The pipeline event was persisted through the store thread.
    assert!(
        wait_until(|| {
            store_for_wait
                .history()
                .count(&FriendHistoryFilter::default())
                .unwrap_or(0)
                >= 1
        })
        .await,
        "friend-history row never arrived"
    );
    let rows = store_for_wait
        .history()
        .list(&FriendHistoryFilter {
            user_id: Some(ONLINE_ID.to_string()),
            ..FriendHistoryFilter::default()
        })
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].user_id, ONLINE_ID);
    assert_eq!(rows[0].display_name, "Test Online");
    assert_eq!(
        rows[0].world_id,
        "wrld_00000000-0000-0000-0000-00000000000d"
    );
    assert_eq!(rows[0].instance_id, "55~region(us)");

    // And queryable through the command path.
    handle
        .commands
        .send(Command::LoadFriendHistory {
            page: 0,
            filter: FriendHistoryFilter {
                user_id: Some(ONLINE_ID.to_string()),
                ..FriendHistoryFilter::default()
            },
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::FriendHistoryLoaded)).await;
    let page = handle.state.snapshot().friend_history.unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.entries[0].user_id, ONLINE_ID);
}

#[tokio::test]
async fn missing_log_dir_and_empty_store_start_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-dir");
    let (config, _store) = headless_config(missing);
    let mut handle = App::spawn(config);
    assert!(!handle.state.snapshot().log_tail_active);

    handle
        .commands
        .send(Command::LoadTimeline {
            page: 0,
            filter: TimelineFilter::default(),
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::TimelineLoaded)).await;
    let page = handle.state.snapshot().timeline.unwrap();
    assert_eq!(page.total, 0);
    assert!(page.entries.is_empty());
}

/// A read-only instance (CLI `timeline`/`stats`) shares `vrc.db` with the
/// desktop app: it must not tail logs, persist rows, or run
/// `CloseAllSessions` over sessions the primary instance owns — while
/// store queries keep working.
#[tokio::test]
async fn read_only_instance_neither_tails_nor_touches_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let (mut config, store) = headless_config(dir.path().to_path_buf());
    config.local_writes = false;

    // Seed an open session the way a previous writer would leave it.
    store
        .sessions()
        .open(&NewSession {
            started_at: time::OffsetDateTime::now_utc() - time::Duration::hours(1),
            world_id: "wrld_seed".into(),
            ..NewSession::default()
        })
        .unwrap();

    let mut handle = App::spawn(config);
    // Let the core thread finish startup (open store, build runtime).
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The seeded session survived: no CloseAllSessions ran.
    assert!(store.sessions().current().unwrap().is_some());
    // No tailer was started.
    assert!(!handle.state.snapshot().log_tail_active);

    // Log lines written now produce no timeline rows.
    std::fs::write(
        dir.path().join("output_log_2024-05-14_20-45-00.txt"),
        "2024.05.14 20:45:00 Log - [Behaviour] Joining wrld_00000000-0000-0000-0000-00000000000a:1~region(us)\n",
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        store.timeline().count(&TimelineFilter::default()).unwrap(),
        0
    );

    // Queries still work — this is the CLI read path.
    handle
        .commands
        .send(Command::LoadTimeline {
            page: 0,
            filter: TimelineFilter::default(),
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::TimelineLoaded)).await;
    assert_eq!(handle.state.snapshot().timeline.unwrap().total, 0);
}

const NOTIF_ID: &str = "not_00000000-0000-0000-0000-000000000001";
const FRQ_ID: &str = "frq_00000000-0000-0000-0000-000000000009";
const INVITE_INSTANCE: &str = "55~hidden(usr_00000000-0000-0000-0000-000000000002)~region(us)";

fn fake_notification(id: &str, kind: &str, sender: &str) -> serde_json::Value {
    json!({
        "id": id,
        "type": kind,
        "created_at": "2026-01-02T03:04:05.000Z",
        "details": "{}",
        "message": "Synthetic notification.",
        "seen": false,
        "senderUserId": sender,
        "senderUsername": "",
        "receiverUserId": "usr_00000000-0000-0000-0000-0000000000ff"
    })
}

fn success_body() -> serde_json::Value {
    json!({"success": {"message": "ok", "status_code": 200}})
}

/// Shared login setup for the social tests: wiremock REST + a WS listener
/// whose frames the test drives through `frames_tx`.
struct SocialHarness {
    handle: AppHandle,
    frames_tx: mpsc::Sender<String>,
}

async fn spawn_social_app(server: &MockServer) -> SocialHarness {
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(CURRENT_USER, "application/json")
                .insert_header("set-cookie", format!("auth={FAKE_COOKIE}; Path=/")),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth/user/friends"))
        .and(query_param("offline", "false"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![fake_friend(
            ONLINE_ID,
            "Test Online",
            "wrld_00000000-0000-0000-0000-00000000000a:inst1",
        )]))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth/user/friends"))
        .and(query_param("offline", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<serde_json::Value>::new()))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/favorites"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<serde_json::Value>::new()))
        .mount(server)
        .await;

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
        data_dir: None,
        log_dir: Some(std::path::PathBuf::from("vrc-core-test-no-such-log-dir")),
        store: Some(vrc_core::Store::open_in_memory().unwrap()),
        local_writes: true,
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
    SocialHarness { handle, frames_tx }
}

#[tokio::test]
async fn social_notifications_and_profile_commands() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user/notifications"))
        .and(query_param("n", "100"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![
            fake_notification(NOTIF_ID, "friendRequest", ONLINE_ID),
            fake_notification(
                "not_00000000-0000-0000-0000-000000000002",
                "invite",
                ONLINE_ID,
            ),
        ]))
        .mount(&server)
        .await;
    for (id, suffix) in [
        (NOTIF_ID, "see"),
        ("not_00000000-0000-0000-0000-000000000002", "hide"),
    ] {
        Mock::given(method("PUT"))
            .and(path(format!("/auth/user/notifications/{id}/{suffix}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_body()))
            .mount(&server)
            .await;
    }
    Mock::given(method("PUT"))
        .and(path(format!("/auth/user/notifications/{FRQ_ID}/accept")))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_body()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/users/{ONLINE_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": ONLINE_ID,
            "name": "testonline",
            "displayName": "Test Online",
            "bio": "Synthetic fixture.",
            "note": "",
            "friendRequestStatus": "none",
            "tags": [],
            "isFriend": true
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/user/{ONLINE_ID}/friendStatus")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "incomingRequest": false,
            "isFriend": true,
            "outgoingRequest": false
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth/user/playermoderations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<serde_json::Value>::new()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/userNotes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "note_00000000-0000-0000-0000-000000000001",
            "createdAt": "2026-01-02T03:04:05.000Z",
            "note": "met at the park",
            "targetUserId": ONLINE_ID,
            "userId": "usr_00000000-0000-0000-0000-0000000000ff"
        })))
        .mount(&server)
        .await;

    let SocialHarness {
        mut handle,
        frames_tx,
    } = spawn_social_app(&server).await;

    // Notifications list loads.
    handle
        .commands
        .send(Command::RefreshNotifications)
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::NotificationsChanged)).await;
    let state = handle.state.snapshot();
    let notifications = state.notifications.clone().unwrap();
    assert_eq!(notifications.len(), 2);
    assert_eq!(notifications[0].id, NOTIF_ID);

    // A pipeline push upserts a third notification without a re-fetch.
    frames_tx
        .send(format!(
            r#"{{"type":"notification","content":{}}}"#,
            serde_json::to_string(&fake_notification(
                "not_00000000-0000-0000-0000-000000000003",
                "message",
                ONLINE_ID
            ))
            .unwrap()
        ))
        .await
        .unwrap();
    wait_for(&mut handle, |e| {
        matches!(e, AppEvent::Pipeline(ev) if matches!(&**ev, PipelineEvent::Notification { .. }))
    })
    .await;
    let state = handle.state.snapshot();
    assert_eq!(state.notifications.as_ref().unwrap().len(), 3);

    // see → seen flag; hide → removed.
    handle
        .commands
        .send(Command::SeeNotification {
            id: NOTIF_ID.to_string(),
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::NotificationsChanged)).await;
    let state = handle.state.snapshot();
    assert!(state.notifications.as_ref().unwrap()[1].seen);

    handle
        .commands
        .send(Command::HideNotification {
            id: "not_00000000-0000-0000-0000-000000000002".to_string(),
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::NotificationsChanged)).await;
    let state = handle.state.snapshot();
    assert_eq!(state.notifications.as_ref().unwrap().len(), 2);

    // accept removes the frq notification (not in list anyway) and
    // refreshes friends.
    handle
        .commands
        .send(Command::AcceptNotification {
            id: FRQ_ID.to_string(),
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::NotificationsChanged)).await;

    // Profile load + note.
    handle
        .commands
        .send(Command::LoadProfile {
            user_id: ONLINE_ID.to_string(),
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::ProfileChanged)).await;
    let state = handle.state.snapshot();
    let profile = state.profile.clone().unwrap();
    assert_eq!(profile.user.id, ONLINE_ID);
    assert!(profile.friend_status.is_friend);

    handle
        .commands
        .send(Command::SetUserNote {
            user_id: ONLINE_ID.to_string(),
            note: "met at the park".to_string(),
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::ProfileChanged)).await;
    assert_eq!(
        handle.state.snapshot().profile.unwrap().user.note,
        "met at the park"
    );

    server.verify().await;
}

#[tokio::test]
async fn social_friend_moderation_and_invite_commands() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/user/{ONLINE_ID}/friendRequest")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_notification(
            FRQ_ID,
            "friendRequest",
            ONLINE_ID,
        )))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("/user/{ONLINE_ID}/friendRequest")))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_body()))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("/auth/user/friends/{ONLINE_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_body()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/users/{ONLINE_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": ONLINE_ID,
            "name": "testonline",
            "displayName": "Test Online",
            "isFriend": true,
            "friendRequestStatus": "none"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/user/{ONLINE_ID}/friendStatus")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "incomingRequest": false,
            "isFriend": true,
            "outgoingRequest": false
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth/user/playermoderations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<serde_json::Value>::new()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/user/playermoderations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "pmod_00000000-0000-0000-0000-000000000001",
            "created": "2026-01-02T03:04:05.000Z",
            "sourceDisplayName": "Me",
            "sourceUserId": "usr_00000000-0000-0000-0000-0000000000ff",
            "targetDisplayName": "Test Online",
            "targetUserId": ONLINE_ID,
            "type": "block"
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/auth/user/unplayermoderate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_body()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/invite/{ONLINE_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_notification(
            "not_00000000-0000-0000-0000-000000000004",
            "invite",
            "usr_00000000-0000-0000-0000-0000000000ff",
        )))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/requestInvite/{ONLINE_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_notification(
            "not_00000000-0000-0000-0000-000000000005",
            "requestInvite",
            ONLINE_ID,
        )))
        .mount(&server)
        .await;

    let SocialHarness { mut handle, .. } = spawn_social_app(&server).await;

    // Profile first so friend/moderation state has somewhere to land.
    handle
        .commands
        .send(Command::LoadProfile {
            user_id: ONLINE_ID.to_string(),
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::ProfileChanged)).await;

    // Friend request lifecycle.
    handle
        .commands
        .send(Command::SendFriendRequest {
            user_id: ONLINE_ID.to_string(),
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::ProfileChanged)).await;
    assert!(
        handle
            .state
            .snapshot()
            .profile
            .unwrap()
            .friend_status
            .outgoing_request
    );
    handle
        .commands
        .send(Command::CancelFriendRequest {
            user_id: ONLINE_ID.to_string(),
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::ProfileChanged)).await;
    assert!(
        !handle
            .state
            .snapshot()
            .profile
            .unwrap()
            .friend_status
            .outgoing_request
    );

    // Unfriend without confirm is rejected before any request; with
    // confirm it removes the friend.
    handle
        .commands
        .send(Command::Unfriend {
            user_id: ONLINE_ID.to_string(),
            confirm: false,
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::Error(_))).await;
    assert!(handle.state.snapshot().friends.contains_key(ONLINE_ID));

    handle
        .commands
        .send(Command::Unfriend {
            user_id: ONLINE_ID.to_string(),
            confirm: true,
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::ProfileChanged)).await;
    assert!(!handle.state.snapshot().friends.contains_key(ONLINE_ID));

    // Block requires confirm; mute does not.
    handle
        .commands
        .send(Command::ModerateUser {
            user_id: ONLINE_ID.to_string(),
            kind: PlayerModerationKind::Block,
            confirm: false,
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::Error(_))).await;
    assert!(!handle
        .state
        .snapshot()
        .profile
        .unwrap()
        .has_moderation(PlayerModerationKind::Block));

    handle
        .commands
        .send(Command::ModerateUser {
            user_id: ONLINE_ID.to_string(),
            kind: PlayerModerationKind::Block,
            confirm: true,
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::ProfileChanged)).await;
    assert!(handle
        .state
        .snapshot()
        .profile
        .unwrap()
        .has_moderation(PlayerModerationKind::Block));

    handle
        .commands
        .send(Command::UnmoderateUser {
            user_id: ONLINE_ID.to_string(),
            kind: PlayerModerationKind::Block,
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::ProfileChanged)).await;
    assert!(!handle
        .state
        .snapshot()
        .profile
        .unwrap()
        .has_moderation(PlayerModerationKind::Block));

    // Invites: explicit instance override (the fixture has no presence).
    handle
        .commands
        .send(Command::InviteUser {
            user_id: ONLINE_ID.to_string(),
            instance_id: Some(INVITE_INSTANCE.to_string()),
            message_slot: Some(2),
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::InviteSent)).await;
    handle
        .commands
        .send(Command::RequestInvite {
            user_id: ONLINE_ID.to_string(),
            request_slot: None,
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::InviteSent)).await;

    server.verify().await;
}

#[tokio::test]
async fn multi_invite_is_throttled_and_cancellable() {
    const TARGET_A: &str = "usr_00000000-0000-0000-0000-0000000000a1";
    const TARGET_B: &str = "usr_00000000-0000-0000-0000-0000000000b2";
    const TARGET_C: &str = "usr_00000000-0000-0000-0000-0000000000c3";

    let server = MockServer::start().await;
    // One matcher covers every /invite/{usr_…} POST.
    Mock::given(method("POST"))
        .and(wiremock::matchers::path_regex(r"^/invite/usr_"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_notification(
            "not_00000000-0000-0000-0000-000000000006",
            "invite",
            "usr_00000000-0000-0000-0000-0000000000ff",
        )))
        .mount(&server)
        .await;

    let SocialHarness { mut handle, .. } = spawn_social_app(&server).await;

    // Two targets run to completion (~1.5 s gap).
    handle
        .commands
        .send(Command::MultiInvite {
            user_ids: vec![TARGET_A.to_string(), TARGET_B.to_string()],
            instance_id: Some(INVITE_INSTANCE.to_string()),
        })
        .await
        .unwrap();
    assert!(
        wait_until(|| {
            handle
                .state
                .snapshot()
                .multi_invite
                .as_ref()
                .is_some_and(|p| !p.running)
        })
        .await,
        "multi-invite never finished"
    );
    let progress = handle.state.snapshot().multi_invite.unwrap();
    assert_eq!(progress.total, 2);
    assert_eq!(progress.done, 2);
    assert_eq!(progress.succeeded, 2);
    assert_eq!(progress.failed, 0);

    // Three targets cancelled mid-flight: at most the in-flight invite
    // completes, the rest are skipped.
    handle
        .commands
        .send(Command::MultiInvite {
            user_ids: vec![
                TARGET_A.to_string(),
                TARGET_B.to_string(),
                TARGET_C.to_string(),
            ],
            instance_id: Some(INVITE_INSTANCE.to_string()),
        })
        .await
        .unwrap();
    // Wait for the first invite to complete (done >= 1 while still
    // running) — that state persists for the whole 1.5 s gap, unlike the
    // millisecond `current` window which a 50 ms poll can miss.
    assert!(
        wait_until(|| {
            handle
                .state
                .snapshot()
                .multi_invite
                .as_ref()
                .is_some_and(|p| p.running && p.done >= 1)
        })
        .await,
        "multi-invite never completed an invite"
    );
    handle
        .commands
        .send(Command::CancelMultiInvite)
        .await
        .unwrap();
    assert!(
        wait_until(|| {
            handle
                .state
                .snapshot()
                .multi_invite
                .as_ref()
                .is_some_and(|p| !p.running)
        })
        .await,
        "cancelled multi-invite never finished"
    );
    let progress = handle.state.snapshot().multi_invite.unwrap();
    assert_eq!(progress.total, 3);
    assert!(progress.done < 3, "cancel did not stop the batch");
    assert_eq!(progress.succeeded + progress.failed, progress.done);

    // Validation errors surface as command errors without requests.
    handle
        .commands
        .send(Command::MultiInvite {
            user_ids: vec!["not_a_user".to_string()],
            instance_id: None,
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::Error(_))).await;

    server.verify().await;
}
