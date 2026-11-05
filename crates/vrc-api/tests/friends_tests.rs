//! Paginated `GET /friends` and `GET /favorites` tests against `wiremock`
//! with synthetic fixtures only (RULES.md §6). No live network.

use serde_json::{json, Value};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use vrc_api::{Client, MemoryVault};

fn test_client(server: &MockServer) -> Client {
    Client::builder()
        .base_url(server.uri())
        .vault(MemoryVault::new())
        .rate_limit(1_000.0, 100)
        .build()
        .expect("client builds")
}

/// Synthetic `LimitedUserFriend` body — fake ids/names only (RULES.md §6).
fn fake_friend(n: usize) -> Value {
    json!({
        "id": format!("usr_00000000-0000-0000-0000-{n:012}"),
        "displayName": format!("Test Friend {n:03}"),
        "userIcon": "",
        "bio": "Synthetic test fixture, not a real account.",
        "bioLinks": [],
        "friendKey": "",
        "friendId": "",
        "last_platform": "standalonewindows",
        "platform": "standalonewindows",
        "currentAvatarImageUrl": "https://api.vrchat.cloud/api/1/file/file_00000000-0000-0000-0000-000000000001/1/file",
        "currentAvatarThumbnailImageUrl": "https://api.vrchat.cloud/api/1/image/file_00000000-0000-0000-0000-000000000001/1/256",
        "currentAvatarTags": [],
        "developerType": "none",
        "status": "active",
        "statusDescription": "synthetic",
        "location": "offline",
        "fallbackAvatar": "",
        "profilePicOverride": "",
        "profilePicOverrideThumbnail": "",
        "isFriend": true,
        "last_login": "2026-01-02T03:04:05.000Z",
        "last_activity": null,
        "tags": ["system_trust_basic", "system_trust_newuser", "language_eng"]
    })
}

fn fake_friends(range: std::ops::Range<usize>) -> Vec<Value> {
    range.map(fake_friend).collect()
}

#[tokio::test]
async fn friends_paginates_until_short_page() {
    let server = MockServer::start().await;
    // Full first page forces one more request; the short second page ends it.
    Mock::given(method("GET"))
        .and(path("/auth/user/friends"))
        .and(query_param("offline", "false"))
        .and(query_param("n", "100"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_friends(0..100)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth/user/friends"))
        .and(query_param("offline", "false"))
        .and(query_param("n", "100"))
        .and(query_param("offset", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_friends(100..102)))
        .expect(1)
        .mount(&server)
        .await;

    let friends = test_client(&server).friends(false).await.unwrap();
    assert_eq!(friends.len(), 102);
    assert_eq!(friends[0].id, "usr_00000000-0000-0000-0000-000000000000");
    assert_eq!(friends[101].display_name, "Test Friend 101");
    server.verify().await;
}

#[tokio::test]
async fn friends_offline_flag_and_null_tolerant_decode() {
    let server = MockServer::start().await;
    // Explicit nulls must not break the decode (the API emits them).
    let mut body = fake_friend(1);
    body["last_login"] = Value::Null;
    body["userIcon"] = Value::Null;
    body["tags"] = Value::Null;
    Mock::given(method("GET"))
        .and(path("/auth/user/friends"))
        .and(query_param("offline", "true"))
        .and(query_param("n", "100"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![body]))
        .expect(1)
        .mount(&server)
        .await;

    let friends = test_client(&server).friends(true).await.unwrap();
    assert_eq!(friends.len(), 1);
    assert_eq!(friends[0].display_name, "Test Friend 001");
    assert!(friends[0].last_login.is_empty());
    server.verify().await;
}

#[tokio::test]
async fn friends_empty_first_page_stops_immediately() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user/friends"))
        .and(query_param("offline", "false"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<Value>::new()))
        .expect(1)
        .mount(&server)
        .await;

    let friends = test_client(&server).friends(false).await.unwrap();
    assert!(friends.is_empty());
    server.verify().await;
}

#[tokio::test]
async fn favorite_friend_ids_collects_friend_type_entries() {
    let server = MockServer::start().await;
    let page = vec![
        json!({
            "id": "fv_00000000-0000-0000-0000-000000000001",
            "type": "friend",
            "favoriteId": "usr_00000000-0000-0000-0000-000000000010",
            "tags": ["group_0"],
            "visibility": "friends"
        }),
        json!({
            "id": "fv_00000000-0000-0000-0000-000000000002",
            "type": "friend",
            "favoriteId": "usr_00000000-0000-0000-0000-000000000011",
            "tags": [],
            "visibility": "private"
        }),
        // A stray non-friend entry is ignored defensively.
        json!({
            "id": "fv_00000000-0000-0000-0000-000000000003",
            "type": "world",
            "favoriteId": "wrld_00000000-0000-0000-0000-000000000001",
            "tags": [],
            "visibility": "private"
        }),
    ];
    Mock::given(method("GET"))
        .and(path("/favorites"))
        .and(query_param("type", "friend"))
        .and(query_param("n", "100"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page))
        .expect(1)
        .mount(&server)
        .await;

    let ids = test_client(&server).favorite_friend_ids().await.unwrap();
    assert_eq!(
        ids,
        vec![
            "usr_00000000-0000-0000-0000-000000000010".to_string(),
            "usr_00000000-0000-0000-0000-000000000011".to_string(),
        ]
    );
    server.verify().await;
}

#[tokio::test]
async fn friends_pagination_stops_at_page_cap() {
    let server = MockServer::start().await;
    // An endpoint that always returns a full page would loop forever
    // without the page cap.
    Mock::given(method("GET"))
        .and(path("/auth/user/friends"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_friends(0..100)))
        .mount(&server)
        .await;

    let friends = test_client(&server).friends(false).await.unwrap();
    assert_eq!(friends.len(), 50 * 100);
    assert_eq!(server.received_requests().await.unwrap().len(), 50);
}
