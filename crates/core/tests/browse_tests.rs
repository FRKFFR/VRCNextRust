//! Headless M5 `vrc-core` tests: wiremock REST + synthetic fixtures only
//! (RULES.md §6). Covers debounced/deduped search, detail caches, group
//! actions, avatar selection guard, favorites and instance commands.

use std::time::Duration;

use secrecy::SecretString;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::time::{sleep, timeout};
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use vrc_core::{
    App, AppConfig, AppEvent, AppHandle, AuthState, Client, Command, CreateInstanceRequest,
    DetailState, FavoriteKind, InstanceKind, InstanceRegion, MemoryVault, PipelineConfig,
};

const CURRENT_USER: &str = include_str!("../../vrc-api/tests/fixtures/current_user.json");
const FAKE_COOKIE: &str = "authcookie_00000000000000000000000000000000";
const WORLD_ID: &str = "wrld_00000000-0000-0000-0000-000000000001";
const GROUP_ID: &str = "grp_00000000-0000-0000-0000-000000000001";
const AVATAR_ID: &str = "avtr_00000000-0000-0000-0000-000000000001";
const INSTANCE_ID: &str = "12345~region(us)";
const WAIT: Duration = Duration::from_secs(15);

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

async fn wait_browse(handle: &mut AppHandle) {
    wait_for(handle, |e| matches!(e, AppEvent::BrowseChanged)).await;
}

fn fake_world() -> serde_json::Value {
    json!({
        "id": WORLD_ID,
        "name": "Synthetic World",
        "authorName": "Test Author",
        "capacity": 32,
        "favorites": 7,
        "releaseStatus": "public"
    })
}

fn fake_group() -> serde_json::Value {
    json!({
        "id": GROUP_ID,
        "groupId": GROUP_ID,
        "name": "Synthetic Group",
        "shortCode": "synth",
        "memberCount": 12,
        "onlineMemberCount": 3,
        "joinState": "open",
        "membershipStatus": "None",
        "galleries": [],
        "roles": []
    })
}

fn fake_instance() -> serde_json::Value {
    json!({
        "id": format!("{WORLD_ID}:{INSTANCE_ID}"),
        "instanceId": INSTANCE_ID,
        "worldId": WORLD_ID,
        "location": format!("{WORLD_ID}:{INSTANCE_ID}"),
        "type": "public",
        "region": "us",
        "capacity": 32,
        "n_users": 1,
        "userCount": 1,
        "active": true,
        "world": fake_world()
    })
}

/// Mount the auth + post-login fetch mocks every test needs, spawn the
/// app, and return the handle. M5 endpoint mocks must be mounted by the
/// caller *before* [`login`] is invoked.
async fn authed_handle(server: &MockServer) -> AppHandle {
    Mock::given(method("GET"))
        .and(path("/auth/user"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(CURRENT_USER, "application/json")
                .insert_header("set-cookie", format!("auth={FAKE_COOKIE}; Path=/")),
        )
        .mount(server)
        .await;
    for offline in ["false", "true"] {
        Mock::given(method("GET"))
            .and(path("/auth/user/friends"))
            .and(query_param("offline", offline))
            .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<serde_json::Value>::new()))
            .mount(server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/favorites"))
        .and(query_param("type", "friend"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<serde_json::Value>::new()))
        .mount(server)
        .await;

    // Local WebSocket endpoint so the pipeline connects like in production.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let _ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
    });

    let client = Client::builder()
        .base_url(server.uri())
        .vault(MemoryVault::new())
        .rate_limit(1_000.0, 100)
        .build()
        .unwrap();
    App::spawn(AppConfig {
        client,
        pipeline: PipelineConfig {
            url: format!("ws://127.0.0.1:{port}"),
            ..PipelineConfig::default()
        },
        data_dir: None,
        log_dir: Some(std::path::PathBuf::from("vrc-core-test-no-such-log-dir")),
        store: Some(vrc_core::Store::open_in_memory().unwrap()),
        local_writes: true,
    })
}

async fn login(handle: &mut AppHandle) {
    handle
        .commands
        .send(Command::Login {
            username: "testuser".to_string(),
            password: SecretString::from("synthetic password"),
        })
        .await
        .unwrap();
    wait_for(handle, |e| {
        matches!(e, AppEvent::AuthChanged(AuthState::Authenticated))
    })
    .await;
    wait_for(handle, |e| matches!(e, AppEvent::FriendsRefreshed)).await;
}

#[tokio::test]
async fn world_search_debounces_and_dedups() {
    let server = MockServer::start().await;
    // Exactly one /worlds request may leave the core: the first command is
    // superseded during the 400 ms debounce window.
    Mock::given(method("GET"))
        .and(path("/worlds"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![fake_world()]))
        .expect(1)
        .mount(&server)
        .await;
    let mut handle = authed_handle(&server).await;
    login(&mut handle).await;

    handle
        .commands
        .send(Command::SearchWorlds {
            query: "first".to_string(),
            offset: 0,
        })
        .await
        .unwrap();
    handle
        .commands
        .send(Command::SearchWorlds {
            query: "second".to_string(),
            offset: 0,
        })
        .await
        .unwrap();
    // Identical in-flight searches dedup too.
    handle
        .commands
        .send(Command::SearchWorlds {
            query: "second".to_string(),
            offset: 0,
        })
        .await
        .unwrap();

    wait_browse(&mut handle).await;
    // Let any stale task finish before asserting the request count.
    sleep(Duration::from_millis(700)).await;
    let state = handle.state.snapshot();
    let page = state.world_search.expect("world search page");
    assert_eq!(page.key, "second");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].name, "Synthetic World");
    server.verify().await;
}

#[tokio::test]
async fn world_detail_caches_and_dedups_in_flight() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/worlds/{WORLD_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_world()))
        .expect(1)
        .mount(&server)
        .await;
    let mut handle = authed_handle(&server).await;
    login(&mut handle).await;

    for _ in 0..2 {
        handle
            .commands
            .send(Command::LoadWorld {
                world_id: WORLD_ID.to_string(),
            })
            .await
            .unwrap();
    }
    wait_browse(&mut handle).await;
    sleep(Duration::from_millis(300)).await;

    let state = handle.state.snapshot();
    let Some(DetailState::Known(world)) = state.world_details.get(WORLD_ID) else {
        panic!("world detail missing");
    };
    assert_eq!(world.name, "Synthetic World");
    server.verify().await;
}

#[tokio::test]
async fn group_detail_join_members_posts_and_instances() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/groups/{GROUP_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_group()))
        .expect(2) // initial load + reload after join
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/groups/{GROUP_ID}/join")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "gmem_1",
            "groupId": GROUP_ID,
            "userId": "usr_1",
            "membershipStatus": "Member"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/groups/{GROUP_ID}/members")))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![json!({
            "id": "gmem_1",
            "groupId": GROUP_ID,
            "userId": "usr_1",
            "membershipStatus": "Member",
            "user": {"id": "usr_1", "displayName": "Test Member"}
        })]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/groups/{GROUP_ID}/posts")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "posts": [{"id": "gpost_1", "groupId": GROUP_ID, "title": "Hi", "text": "Synthetic post."}],
            "total": 1
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/groups/{GROUP_ID}/instances")))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![json!({
            "instanceId": INSTANCE_ID,
            "location": format!("{WORLD_ID}:{INSTANCE_ID}"),
            "memberCount": 4,
            "groupId": GROUP_ID,
            "world": fake_world()
        })]))
        .expect(1)
        .mount(&server)
        .await;

    let mut handle = authed_handle(&server).await;
    login(&mut handle).await;

    handle
        .commands
        .send(Command::LoadGroup {
            group_id: GROUP_ID.to_string(),
            include_roles: true,
        })
        .await
        .unwrap();
    wait_browse(&mut handle).await;
    handle
        .commands
        .send(Command::JoinGroup {
            group_id: GROUP_ID.to_string(),
            confirm_override_block: false,
        })
        .await
        .unwrap();
    // Join triggers a detail reload → another BrowseChanged.
    wait_browse(&mut handle).await;
    handle
        .commands
        .send(Command::LoadGroupMembers {
            group_id: GROUP_ID.to_string(),
            offset: 0,
        })
        .await
        .unwrap();
    wait_browse(&mut handle).await;
    handle
        .commands
        .send(Command::LoadGroupPosts {
            group_id: GROUP_ID.to_string(),
            offset: 0,
            public_only: false,
        })
        .await
        .unwrap();
    wait_browse(&mut handle).await;
    handle
        .commands
        .send(Command::LoadGroupInstances {
            group_id: GROUP_ID.to_string(),
        })
        .await
        .unwrap();
    wait_browse(&mut handle).await;

    let state = handle.state.snapshot();
    assert!(matches!(
        state.group_details.get(GROUP_ID),
        Some(DetailState::Known(_))
    ));
    assert_eq!(state.group_members[GROUP_ID].items.len(), 1);
    assert_eq!(state.group_posts[GROUP_ID].items.len(), 1);
    assert_eq!(state.group_posts[GROUP_ID].total, Some(1));
    assert_eq!(state.group_instances[GROUP_ID].items.len(), 1);
    server.verify().await;
}

#[tokio::test]
async fn avatar_select_requires_confirmation() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path(format!("/avatars/{AVATAR_ID}/select")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(CURRENT_USER, "application/json"))
        .expect(1)
        .mount(&server)
        .await;
    let mut handle = authed_handle(&server).await;
    login(&mut handle).await;

    // Unconfirmed selection is rejected locally — no request, an Error.
    handle
        .commands
        .send(Command::SelectAvatar {
            avatar_id: AVATAR_ID.to_string(),
            confirm: false,
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::Error(_))).await;

    handle
        .commands
        .send(Command::SelectAvatar {
            avatar_id: AVATAR_ID.to_string(),
            confirm: true,
        })
        .await
        .unwrap();
    wait_browse(&mut handle).await;
    assert!(handle.state.snapshot().current_user.is_some());
    server.verify().await;
}

#[tokio::test]
async fn favorites_refresh_add_and_remove() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/worlds/favorites"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![fake_world()]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/avatars/favorites"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<serde_json::Value>::new()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/favorites"))
        .and(query_param("type", "world"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![json!({
            "id": "fvrt_00000000-0000-0000-0000-000000000001",
            "type": "world",
            "favoriteId": WORLD_ID,
            "tags": [],
            "visibility": "friends"
        })]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/favorites"))
        .and(query_param("type", "avatar"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<serde_json::Value>::new()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/favorites"))
        .and(body_json(json!({
            "type": "world",
            "favoriteId": "wrld_00000000-0000-0000-0000-000000000002",
            "tags": []
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "fvrt_00000000-0000-0000-0000-000000000002",
            "type": "world",
            "favoriteId": "wrld_00000000-0000-0000-0000-000000000002",
            "tags": [],
            "visibility": "friends"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/favorites/fvrt_00000000-0000-0000-0000-000000000001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": {"message": "ok", "status_code": 200}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut handle = authed_handle(&server).await;
    login(&mut handle).await;

    handle
        .commands
        .send(Command::RefreshFavorites)
        .await
        .unwrap();
    wait_browse(&mut handle).await;
    {
        let state = handle.state.snapshot();
        assert_eq!(state.favorite_records.len(), 1);
        assert_eq!(state.favorite_worlds.as_ref().unwrap().items.len(), 1);
        assert!(vrc_core::browse::is_favorite(
            &state,
            FavoriteKind::World,
            WORLD_ID
        ));
    }

    // Add a second world favorite; the record list updates immediately.
    handle
        .commands
        .send(Command::AddFavorite {
            kind: FavoriteKind::World,
            favorite_id: "wrld_00000000-0000-0000-0000-000000000002".to_string(),
            tags: Vec::new(),
        })
        .await
        .unwrap();
    wait_browse(&mut handle).await;
    assert_eq!(handle.state.snapshot().favorite_records.len(), 2);

    // Removing resolves the fvrt_ id from the loaded records.
    handle
        .commands
        .send(Command::RemoveFavorite {
            kind: FavoriteKind::World,
            object_id: WORLD_ID.to_string(),
        })
        .await
        .unwrap();
    wait_browse(&mut handle).await;
    let state = handle.state.snapshot();
    assert_eq!(state.favorite_records.len(), 1);
    assert!(!vrc_core::browse::is_favorite(
        &state,
        FavoriteKind::World,
        WORLD_ID
    ));
    server.verify().await;
}

#[tokio::test]
async fn create_instance_and_self_invite() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/instances"))
        .and(body_json(json!({
            "worldId": WORLD_ID,
            "type": "public",
            "region": "us"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_instance()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/invite/myself/to/{WORLD_ID}:{INSTANCE_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "not_00000000-0000-0000-0000-000000000009",
            "type": "invite",
            "created_at": "2026-01-02T03:04:05.000Z",
            "details": "{}",
            "message": "Synthetic invite.",
            "seen": false
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut handle = authed_handle(&server).await;
    login(&mut handle).await;

    handle
        .commands
        .send(Command::CreateInstance {
            request: CreateInstanceRequest::new(WORLD_ID, InstanceKind::Public, InstanceRegion::Us),
        })
        .await
        .unwrap();
    wait_browse(&mut handle).await;
    assert_eq!(
        handle
            .state
            .snapshot()
            .instance
            .as_ref()
            .unwrap()
            .instance_id,
        INSTANCE_ID
    );

    handle
        .commands
        .send(Command::InviteMyselfToInstance {
            world_id: WORLD_ID.to_string(),
            instance_id: INSTANCE_ID.to_string(),
        })
        .await
        .unwrap();
    wait_for(&mut handle, |e| matches!(e, AppEvent::InviteSent)).await;
    server.verify().await;
}
