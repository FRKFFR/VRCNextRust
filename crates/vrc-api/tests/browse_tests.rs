//! M5 endpoint tests against `wiremock` with synthetic fixtures only
//! (RULES.md §6): worlds/groups/avatars search+detail, favorites,
//! instance creation and self-invite. Invalid ids must be rejected before
//! any request is sent.

use serde_json::json;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use vrc_api::{
    Client, CreateInstanceRequest, FavoriteKind, GroupAccessType, InstanceKind, InstanceRegion,
    MemoryVault,
};

const WORLD_ID: &str = "wrld_00000000-0000-0000-0000-000000000001";
const GROUP_ID: &str = "grp_00000000-0000-0000-0000-000000000001";
const GALLERY_ID: &str = "ggal_00000000-0000-0000-0000-000000000001";
const ROLE_ID: &str = "grol_00000000-0000-0000-0000-000000000001";
const AVATAR_ID: &str = "avtr_00000000-0000-0000-0000-000000000001";
const USER_ID: &str = "usr_00000000-0000-0000-0000-000000000001";
const FAVT_ID: &str = "fvrt_00000000-0000-0000-0000-000000000001";
const INSTANCE_ID: &str = "12345~region(us)";

fn test_client(server: &MockServer) -> Client {
    Client::builder()
        .base_url(server.uri())
        .vault(MemoryVault::new())
        .rate_limit(1_000.0, 100)
        .build()
        .expect("client builds")
}

fn fake_world() -> serde_json::Value {
    json!({
        "id": WORLD_ID,
        "name": "Synthetic World",
        "authorName": "Test Author",
        "capacity": 32,
        "favorites": 7,
        "releaseStatus": "public",
        "platforms": ["standalonewindows"]
    })
}

fn fake_group() -> serde_json::Value {
    json!({
        "id": GROUP_ID,
        "groupId": GROUP_ID,
        "name": "Synthetic Group",
        "shortCode": "synth",
        "description": "Synthetic fixture.",
        "memberCount": 12,
        "onlineMemberCount": 3,
        "joinState": "open",
        "membershipStatus": "None",
        "isSearchable": true,
        "galleries": [{"id": GALLERY_ID, "name": "Shots"}],
        "roles": []
    })
}

fn fake_avatar() -> serde_json::Value {
    json!({
        "id": AVATAR_ID,
        "name": "Synthetic Avatar",
        "authorName": "Test Author",
        "authorId": USER_ID,
        "releaseStatus": "public",
        "version": 3
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

#[tokio::test]
async fn world_search_encodes_query_and_pages() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/worlds"))
        .and(query_param("search", "castle & keep"))
        .and(query_param("n", "24"))
        .and(query_param("offset", "48"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![fake_world()]))
        .expect(1)
        .mount(&server)
        .await;

    let worlds = test_client(&server)
        .search_worlds(&vrc_api::WorldSearchQuery {
            search: Some("castle & keep".to_string()),
            n: Some(24),
            offset: Some(48),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(worlds.len(), 1);
    assert_eq!(worlds[0].name, "Synthetic World");
    server.verify().await;
}

#[tokio::test]
async fn world_search_rejects_bad_user_scope_without_request() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    for scope in ["otheruser", "usr_123", "../x"] {
        let err = client
            .search_worlds(&vrc_api::WorldSearchQuery {
                user: Some(scope.to_string()),
                ..Default::default()
            })
            .await
            .expect_err("bad user scope must fail");
        assert!(matches!(err, vrc_api::ApiError::InvalidId(_)));
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn active_and_recent_worlds_hit_documented_paths() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/worlds/active"))
        .and(query_param("search", "castle"))
        .and(query_param("n", "24"))
        .and(query_param("offset", "24"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![fake_world()]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/worlds/recent"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![fake_world()]))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let query = vrc_api::WorldSearchQuery {
        search: Some("castle".to_string()),
        n: Some(24),
        offset: Some(24),
        ..Default::default()
    };
    let active = client.active_worlds(&query).await.unwrap();
    assert_eq!(active[0].name, "Synthetic World");
    let recent = client.recent_worlds(&Default::default()).await.unwrap();
    assert_eq!(recent.len(), 1);
    server.verify().await;
}

#[tokio::test]
async fn group_endpoints_hit_documented_paths() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/groups"))
        .and(query_param("query", "synth"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![fake_group()]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/groups/{GROUP_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_group()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/groups/{GROUP_ID}/join")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "gmem_00000000-0000-0000-0000-000000000001",
            "groupId": GROUP_ID,
            "userId": USER_ID,
            "membershipStatus": "Member"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/groups/{GROUP_ID}/members")))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![json!({
            "id": "gmem_00000000-0000-0000-0000-000000000001",
            "groupId": GROUP_ID,
            "userId": USER_ID,
            "membershipStatus": "Member",
            "user": {"id": USER_ID, "displayName": "Test User"}
        })]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/groups/{GROUP_ID}/posts")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "posts": [{"id": "gpost_1", "groupId": GROUP_ID, "title": "Hi", "text": "Synthetic post."}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/groups/{GROUP_ID}/galleries/{GALLERY_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![json!({
            "id": "ggimg_1",
            "groupId": GROUP_ID,
            "galleryId": GALLERY_ID,
            "fileId": "file_1",
            "imageUrl": "https://example.invalid/x.png",
            "approved": true
        })]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/groups/{GROUP_ID}/instances")))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![json!({
            "instanceId": INSTANCE_ID,
            "location": format!("{WORLD_ID}:{INSTANCE_ID}"),
            "memberCount": 4,
            "world": fake_world()
        })]))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let groups = client
        .search_groups(Some("synth"), None, None)
        .await
        .unwrap();
    assert_eq!(groups[0].name, "Synthetic Group");
    let group = client.group(GROUP_ID, true).await.unwrap();
    assert_eq!(group.short_code, "synth");
    client.join_group(GROUP_ID).await.unwrap();
    let members = client
        .group_members(GROUP_ID, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(members[0].user_id, USER_ID);
    let posts = client
        .group_posts(GROUP_ID, None, None, false)
        .await
        .unwrap();
    assert_eq!(posts.posts.len(), 1);
    let images = client
        .group_gallery_images(GROUP_ID, GALLERY_ID, None, None, None)
        .await
        .unwrap();
    assert!(images[0].approved);
    let instances = client.group_instances(GROUP_ID).await.unwrap();
    assert_eq!(instances[0].member_count, 4);
    server.verify().await;
}

#[tokio::test]
async fn group_leave_posts_member_and_user_groups_endpoints() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/groups/{GROUP_ID}/leave")))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/groups/{GROUP_ID}/members/{USER_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "gmem_00000000-0000-0000-0000-000000000001",
            "groupId": GROUP_ID,
            "userId": USER_ID,
            "membershipStatus": "Member"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/users/{USER_ID}/groups")))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![json!({
            "groupId": GROUP_ID,
            "id": "gmem_00000000-0000-0000-0000-000000000001",
            "name": "Synthetic Group",
            "shortCode": "synth",
            "memberCount": 12,
            "isRepresenting": false
        })]))
        .expect(1)
        .mount(&server)
        .await;
    let client = test_client(&server);
    client.leave_group(GROUP_ID).await.unwrap();
    let member = client.group_member(GROUP_ID, USER_ID).await.unwrap();
    assert_eq!(member.user_id, USER_ID);
    let groups = client.user_groups(USER_ID).await.unwrap();
    assert_eq!(groups[0].group_id, GROUP_ID);
    assert_eq!(groups[0].short_code, "synth");
    server.verify().await;
}

#[tokio::test]
async fn group_calls_reject_bad_ids_without_request() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    for bad in ["usr_123", "grp_../x", "grp_a?b=c", "grp_a b", ""] {
        assert!(client.group(bad, false).await.is_err(), "{bad}");
        assert!(client.join_group(bad).await.is_err(), "{bad}");
        assert!(client.leave_group(bad).await.is_err(), "{bad}");
        assert!(
            client
                .group_members(bad, None, None, None, None)
                .await
                .is_err(),
            "{bad}"
        );
        assert!(
            client.group_posts(bad, None, None, false).await.is_err(),
            "{bad}"
        );
        assert!(client.group_instances(bad).await.is_err(), "{bad}");
    }
    assert!(client
        .group_gallery_images(GROUP_ID, "grp_notagallery", None, None, None)
        .await
        .is_err());
    assert!(client
        .group_members(GROUP_ID, None, None, None, Some("grol_../x"))
        .await
        .is_err());
    for bad in ["grp_123", "usr_../x", ""] {
        assert!(client.user_groups(bad).await.is_err(), "{bad}");
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn avatar_search_detail_own_and_select() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/avatars"))
        .and(query_param("featured", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![fake_avatar()]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/avatars"))
        .and(query_param("user", "me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<serde_json::Value>::new()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/avatars/{AVATAR_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_avatar()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/users/{USER_ID}/avatar")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_avatar()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("/avatars/{AVATAR_ID}/select")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": USER_ID,
            "displayName": "Test User",
            "currentAvatar": AVATAR_ID
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let featured = client
        .search_avatars(&vrc_api::AvatarSearchQuery {
            featured: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(featured[0].name, "Synthetic Avatar");
    let own = client
        .search_avatars(&vrc_api::AvatarSearchQuery {
            user: Some("me".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(own.is_empty());
    let avatar = client.avatar(AVATAR_ID).await.unwrap();
    assert_eq!(avatar.version, 3);
    let current = client.own_avatar(USER_ID).await.unwrap();
    assert_eq!(current.id, AVATAR_ID);
    let user = client.select_avatar(AVATAR_ID).await.unwrap();
    assert_eq!(user.id, USER_ID);
    server.verify().await;
}

#[tokio::test]
async fn avatar_calls_reject_bad_ids_and_scopes_without_request() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    for bad in ["usr_123", "avtr_../x", "avtr_a?b=c", "avtr_a b", ""] {
        assert!(client.avatar(bad).await.is_err(), "{bad}");
        assert!(client.select_avatar(bad).await.is_err(), "{bad}");
    }
    assert!(client
        .search_avatars(&vrc_api::AvatarSearchQuery {
            user: Some("someone_else".to_string()),
            ..Default::default()
        })
        .await
        .is_err());
    assert!(client.own_avatar("grp_123").await.is_err());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn favorites_list_add_and_remove() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/worlds/favorites"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![fake_world()]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/avatars/favorites"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![fake_avatar()]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/favorites"))
        .and(query_param("type", "world"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![json!({
            "id": FAVT_ID,
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
    Mock::given(method("GET"))
        .and(path("/favorites"))
        .and(query_param("type", "friend"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![json!({
            "id": "fvrt_00000000-0000-0000-0000-000000000003",
            "type": "friend",
            "favoriteId": USER_ID,
            "tags": [],
            "visibility": "friends"
        })]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/favorite/groups"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![json!({
            "id": "fvgrp_00000000-0000-0000-0000-000000000001",
            "displayName": "Close friends",
            "name": "group_0",
            "ownerId": USER_ID,
            "ownerDisplayName": "Test User",
            "type": "friend",
            "visibility": "friends",
            "tags": []
        })]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/favorites"))
        .and(body_json(json!({
            "type": "avatar",
            "favoriteId": AVATAR_ID,
            "tags": ["cool"]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "fvrt_00000000-0000-0000-0000-000000000002",
            "type": "avatar",
            "favoriteId": AVATAR_ID,
            "tags": ["cool"],
            "visibility": "friends"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("/favorites/{FAVT_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": {"message": "ok", "status_code": 200}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let worlds = client.favorite_worlds(None, None, None).await.unwrap();
    assert_eq!(worlds[0].id, WORLD_ID);
    let avatars = client.favorite_avatars(None, None, None).await.unwrap();
    assert_eq!(avatars[0].id, AVATAR_ID);
    let records = client
        .favorites(FavoriteKind::World, None, None)
        .await
        .unwrap();
    assert_eq!(records[0].favorite_id, WORLD_ID);
    let avatar_records = client
        .favorites(FavoriteKind::Avatar, None, None)
        .await
        .unwrap();
    assert!(avatar_records.is_empty());
    let friend_records = client
        .favorites(FavoriteKind::Friend, None, None)
        .await
        .unwrap();
    assert_eq!(friend_records[0].favorite_id, USER_ID);
    let groups = client.favorite_groups(None, None, None).await.unwrap();
    assert_eq!(groups[0].display_name, "Close friends");
    assert_eq!(groups[0].kind, "friend");
    let added = client
        .add_favorite(FavoriteKind::Avatar, AVATAR_ID, &["cool".to_string()])
        .await
        .unwrap();
    assert_eq!(added.tags, ["cool"]);
    client.remove_favorite(FAVT_ID).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn favorites_reject_bad_ids_without_request() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    for bad in ["usr_123", "avtr_../x", "wrld_a?b=c", ""] {
        assert!(
            client
                .add_favorite(FavoriteKind::World, bad, &[])
                .await
                .is_err(),
            "{bad}"
        );
        assert!(
            client
                .add_favorite(FavoriteKind::Avatar, bad, &[])
                .await
                .is_err(),
            "{bad}"
        );
    }
    assert!(client.remove_favorite("fv_123").await.is_err());
    assert!(client.remove_favorite("fvrt_../x").await.is_err());
    assert!(client
        .favorite_groups(None, None, Some("grp_123"))
        .await
        .is_err());
    assert!(client
        .favorite_groups(None, None, Some("usr_../x"))
        .await
        .is_err());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn create_instance_posts_documented_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/instances"))
        .and(body_json(json!({
            "worldId": WORLD_ID,
            "type": "group",
            "region": "eu",
            "ownerId": GROUP_ID,
            "groupAccessType": "members",
            "roleIds": [ROLE_ID]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_instance()))
        .expect(1)
        .mount(&server)
        .await;

    let mut request = CreateInstanceRequest::new(WORLD_ID, InstanceKind::Group, InstanceRegion::Eu);
    request.owner_id = Some(GROUP_ID.to_string());
    request.group_access_type = Some(GroupAccessType::Members);
    request.role_ids = vec![ROLE_ID.to_string()];

    let instance = test_client(&server)
        .create_instance(&request)
        .await
        .unwrap();
    assert_eq!(instance.world_id, WORLD_ID);
    server.verify().await;
}

#[tokio::test]
async fn create_instance_validates_before_request() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    // Group kind without ownerId.
    let bad = CreateInstanceRequest::new(WORLD_ID, InstanceKind::Group, InstanceRegion::Us);
    assert!(client.create_instance(&bad).await.is_err());
    // Group kind with a user ownerId.
    let mut bad2 = CreateInstanceRequest::new(WORLD_ID, InstanceKind::Group, InstanceRegion::Us);
    bad2.owner_id = Some(USER_ID.to_string());
    assert!(client.create_instance(&bad2).await.is_err());
    // Bad world id.
    let bad3 = CreateInstanceRequest::new("wrld_../x", InstanceKind::Public, InstanceRegion::Us);
    assert!(client.create_instance(&bad3).await.is_err());
    // groupAccessType on a non-group instance.
    let mut bad4 = CreateInstanceRequest::new(WORLD_ID, InstanceKind::Public, InstanceRegion::Us);
    bad4.group_access_type = Some(GroupAccessType::Public);
    assert!(client.create_instance(&bad4).await.is_err());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn self_invite_posts_to_documented_path() {
    let server = MockServer::start().await;
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

    let notification = test_client(&server)
        .invite_myself_to_instance(WORLD_ID, INSTANCE_ID)
        .await
        .unwrap();
    assert_eq!(notification.id, "not_00000000-0000-0000-0000-000000000009");
    server.verify().await;
}

#[tokio::test]
async fn self_invite_rejects_bad_ids_without_request() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    for (world, inst) in [
        ("usr_123", INSTANCE_ID),
        (WORLD_ID, "a/b"),
        (WORLD_ID, "a?b=c"),
        (WORLD_ID, "a..b"),
        (WORLD_ID, ""),
    ] {
        assert!(
            client.invite_myself_to_instance(world, inst).await.is_err(),
            "{world}:{inst}"
        );
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
}
