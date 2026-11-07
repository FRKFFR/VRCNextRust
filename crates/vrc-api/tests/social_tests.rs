//! Social-action endpoint tests against `wiremock` with synthetic bodies
//! only (RULES.md §6). Invalid ids must be rejected before any request is
//! sent — asserted via `received_requests`.

use serde_json::{json, Value};
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use vrc_api::{Client, MemoryVault, PlayerModerationKind};

const USER_ID: &str = "usr_00000000-0000-0000-0000-000000000001";
const NOTIF_ID: &str = "not_00000000-0000-0000-0000-000000000001";
const FRQ_ID: &str = "frq_00000000-0000-0000-0000-000000000001";
const WORLD_ID: &str = "wrld_00000000-0000-0000-0000-000000000001";
const INSTANCE_ID: &str =
    "12345~hidden(usr_00000000-0000-0000-0000-000000000001)~region(eu)~nonce(abc)";

fn test_client(server: &MockServer) -> Client {
    Client::builder()
        .base_url(server.uri())
        .vault(MemoryVault::new())
        .rate_limit(1_000.0, 100)
        .build()
        .expect("client builds")
}

fn fake_notification() -> Value {
    json!({
        "id": NOTIF_ID,
        "type": "friendRequest",
        "created_at": "2026-01-02T03:04:05.000Z",
        "details": "{}",
        "message": "Synthetic test notification.",
        "seen": false,
        "senderUserId": USER_ID,
        "senderUsername": "",
        "receiverUserId": "usr_00000000-0000-0000-0000-000000000002"
    })
}

fn fake_user() -> Value {
    json!({
        "id": USER_ID,
        "name": "testuser",
        "displayName": "Test User",
        "bio": "Synthetic fixture, not a real account.",
        "status": "active",
        "statusDescription": "",
        "currentAvatarImageUrl": "",
        "currentAvatarThumbnailImageUrl": "",
        "profilePicOverride": "",
        "profilePicOverrideThumbnail": "",
        "userIcon": "",
        "image": "",
        "bannerImage": "",
        "bannerImageUrl": "",
        "avatarImageUrl": "",
        "avatarThumbnailImageUrl": "",
        "profilePicOverrideImageUrl": "",
        "profilePicOverrideThumbnailImageUrl": "",
        "currentAvatar": "",
        "currentAvatarAssetUrl": "",
        "last_login": "2026-01-02T03:04:05.000Z",
        "last_platform": "standalonewindows",
        "last_activity": null,
        "lastLocation": "",
        "lastLocationDestination": "",
        "isFriend": false,
        "friendRequestStatus": "none",
        "note": "",
        "pronouns": "",
        "tags": [],
        "developerType": "none",
        "date_joined": "2020-01-01T00:00:00.000Z",
        "organization": "",
        "isSelf": false,
        "trustLevel": "Trusted User",
        "state": "active",
        "bioLinks": [],
        "ageVerificationStatus": "unknown",
        "hasPendingEmailVerification": false
    })
}

fn fake_instance() -> Value {
    let mut body = json!({
        "id": format!("{WORLD_ID}:{INSTANCE_ID}"),
        "instanceId": INSTANCE_ID,
        "worldId": WORLD_ID,
        "name": "synthetic_instance",
        "displayName": "Synthetic Instance",
        "secureName": "",
        "shortName": "synt",
        "capacity": 32,
        "recommendedCapacity": 16,
        "n_users": 2,
        "userCount": 2,
        "region": "us",
        "photonRegion": "us",
        "type": "private",
        "ownerId": USER_ID,
        "location": format!("{WORLD_ID}:{INSTANCE_ID}"),
        "strict": false,
        "permanent": false,
        "active": true,
        "full": false,
        "queueEnabled": false,
        "queueSize": 0,
        "hasCapacityForYou": true,
        "canRequestInvite": false,
        "ageGate": null,
        "closedAt": null,
        "tags": [],
        "users": [],
    });
    body["world"] = json!({
        "id": WORLD_ID,
        "name": "synthetic_world",
        "authorName": "Test Author",
        "authorId": USER_ID,
        "description": "Synthetic fixture.",
        "imageUrl": "",
        "thumbnailImageUrl": "",
        "releaseStatus": "public",
        "version": 1,
        "capacity": 32,
        "recommendedCapacity": 16,
        "instances": [],
        "unityPackages": [],
        "labsPublicationDate": "2026-01-01T00:00:00.000Z",
        "created_at": "2026-01-01T00:00:00.000Z",
        "updated_at": "2026-01-02T00:00:00.000Z",
        "organization": "",
        "publicTags": [],
        "contentRanked": false,
        "isCurated": false,
        "popularityRank": 0,
        "favCount": 0,
        "isFeatured": false,
        "isStaffFeatured": false
    });
    body
}

#[tokio::test]
async fn notifications_list_decodes_string_details_and_nulls() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user/notifications"))
        .and(query_param("n", "100"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![fake_notification()]))
        .expect(1)
        .mount(&server)
        .await;

    let list = test_client(&server).notifications().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, NOTIF_ID);
    assert!(list[0].is_friend_request());
    assert_eq!(list[0].details, json!({}));
    server.verify().await;
}

#[tokio::test]
async fn notification_actions_hit_documented_put_paths() {
    let server = MockServer::start().await;
    for (suffix, expect_id) in [("see", NOTIF_ID), ("accept", FRQ_ID)] {
        Mock::given(method("PUT"))
            .and(path(format!(
                "/auth/user/notifications/{expect_id}/{suffix}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": {"message": "ok", "status_code": 200}
            })))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("PUT"))
        .and(path(format!("/auth/user/notifications/{NOTIF_ID}/hide")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": NOTIF_ID,
            "type": "invite"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/auth/user/notifications/clear"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": {"message": "ok", "status_code": 200}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    client.see_notification(NOTIF_ID).await.unwrap();
    client.hide_notification(NOTIF_ID).await.unwrap();
    client.accept_friend_request(FRQ_ID).await.unwrap();
    client.clear_notifications().await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn notification_rejects_bad_ids_without_request() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    for bad in ["usr_123", "not_../x", "not_a?b=c", "", "not_\u{1f600}"] {
        assert!(client.notification(bad).await.is_err(), "{bad}");
        assert!(client.see_notification(bad).await.is_err(), "{bad}");
        assert!(client.hide_notification(bad).await.is_err(), "{bad}");
        assert!(client.accept_friend_request(bad).await.is_err(), "{bad}");
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn user_profile_friend_status_and_note_roundtrip() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/users/{USER_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_user()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/user/{USER_ID}/friendStatus")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "incomingRequest": false,
            "isFriend": true,
            "outgoingRequest": false
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/userNotes"))
        .and(body_json(
            json!({"targetUserId": USER_ID, "note": "hi there"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "note_00000000-0000-0000-0000-000000000001",
            "createdAt": "2026-01-02T03:04:05.000Z",
            "note": "hi there",
            "targetUserId": USER_ID,
            "userId": "usr_00000000-0000-0000-0000-000000000002"
        })))
        .expect(1)
        .mount(&server)
        .await;
    // Clearing a note returns a bare string, not a UserNote object.
    Mock::given(method("POST"))
        .and(path("/userNotes"))
        .and(body_json(json!({"targetUserId": USER_ID, "note": ""})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!("User note cleared.")))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let user = client.user(USER_ID).await.unwrap();
    assert_eq!(user.display_name, "Test User");
    assert!(!user.friend_request_status.is_empty() || user.friend_request_status == "none");
    let status = client.friend_status(USER_ID).await.unwrap();
    assert!(status.is_friend);
    let note = client.set_user_note(USER_ID, "hi there").await.unwrap();
    assert_eq!(note.note, "hi there");
    let cleared = client.set_user_note(USER_ID, "").await.unwrap();
    assert_eq!(cleared.target_user_id, USER_ID);
    assert!(cleared.note.is_empty());
    server.verify().await;
}

#[tokio::test]
async fn friend_request_lifecycle_and_unfriend_paths() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/user/{USER_ID}/friendRequest")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_notification()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("/user/{USER_ID}/friendRequest")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": {"message": "ok", "status_code": 200}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("/auth/user/friends/{USER_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": {"message": "ok", "status_code": 200}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let notif = client.send_friend_request(USER_ID).await.unwrap();
    assert_eq!(notif.id, NOTIF_ID);
    client.cancel_friend_request(USER_ID).await.unwrap();
    client.unfriend(USER_ID).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn user_actions_reject_bad_ids_without_request() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    for bad in ["not_123", "usr_../x", "usr_a?b=c", "grp_123"] {
        assert!(client.user(bad).await.is_err(), "{bad}");
        assert!(client.friend_status(bad).await.is_err(), "{bad}");
        assert!(client.send_friend_request(bad).await.is_err(), "{bad}");
        assert!(client.cancel_friend_request(bad).await.is_err(), "{bad}");
        assert!(client.unfriend(bad).await.is_err(), "{bad}");
        assert!(client.set_user_note(bad, "x").await.is_err(), "{bad}");
        assert!(
            client
                .moderate_user(bad, PlayerModerationKind::Mute)
                .await
                .is_err(),
            "{bad}"
        );
        assert!(
            client
                .unmoderate_user(bad, PlayerModerationKind::Mute)
                .await
                .is_err(),
            "{bad}"
        );
        assert!(
            client.invite_user(bad, INSTANCE_ID, None).await.is_err(),
            "{bad}"
        );
        assert!(client.request_invite(bad, None).await.is_err(), "{bad}");
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn invites_hit_documented_paths_with_json_bodies() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/invite/{USER_ID}")))
        .and(body_json(
            json!({"instanceId": INSTANCE_ID, "messageSlot": 3}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_notification()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/requestInvite/{USER_ID}")))
        .and(body_json(json!({"requestSlot": 0})))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_notification()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/invite/{NOTIF_ID}/response")))
        .and(body_json(json!({"responseSlot": 2})))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_notification()))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    client
        .invite_user(USER_ID, INSTANCE_ID, Some(3))
        .await
        .unwrap();
    client.request_invite(USER_ID, None).await.unwrap();
    client.respond_invite(NOTIF_ID, 2).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn invite_rejects_bad_ids_slots_and_instances_without_request() {
    let server = MockServer::start().await;
    let client = test_client(&server);
    assert!(client.invite_user(USER_ID, "a/b", None).await.is_err());
    assert!(client.invite_user(USER_ID, "a?b=c", None).await.is_err());
    assert!(client
        .invite_user(USER_ID, INSTANCE_ID, Some(12))
        .await
        .is_err());
    assert!(client.request_invite(USER_ID, Some(99)).await.is_err());
    assert!(client.respond_invite("usr_123", 1).await.is_err());
    assert!(client.respond_invite(NOTIF_ID, 42).await.is_err());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn moderation_paths_use_documented_kinds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auth/user/playermoderations"))
        .and(query_param("targetUserId", USER_ID))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": "pmod_00000000-0000-0000-0000-000000000001",
            "created": "2026-01-02T03:04:05.000Z",
            "sourceDisplayName": "Me",
            "sourceUserId": "usr_00000000-0000-0000-0000-000000000002",
            "targetDisplayName": "Test User",
            "targetUserId": USER_ID,
            "type": "mute"
        }])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/user/playermoderations"))
        .and(body_json(json!({"moderated": USER_ID, "type": "block"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "pmod_00000000-0000-0000-0000-000000000002",
            "created": "2026-01-02T03:04:05.000Z",
            "sourceDisplayName": "Me",
            "sourceUserId": "usr_00000000-0000-0000-0000-000000000002",
            "targetDisplayName": "Test User",
            "targetUserId": USER_ID,
            "type": "block"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/auth/user/unplayermoderate"))
        .and(body_json(json!({"moderated": USER_ID, "type": "mute"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": {"message": "ok", "status_code": 200}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let mods = client
        .player_moderations(Some(USER_ID), None)
        .await
        .unwrap();
    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].kind(), Some(PlayerModerationKind::Mute));
    client
        .moderate_user(USER_ID, PlayerModerationKind::Block)
        .await
        .unwrap();
    client
        .unmoderate_user(USER_ID, PlayerModerationKind::Mute)
        .await
        .unwrap();
    server.verify().await;
}

#[tokio::test]
async fn instance_fetch_validates_ids_before_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/instances/{WORLD_ID}:{INSTANCE_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_instance()))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let instance = client.instance(WORLD_ID, INSTANCE_ID).await.unwrap();
    assert_eq!(instance.instance_id, INSTANCE_ID);
    assert_eq!(instance.world.name, "synthetic_world");
    assert!(!instance.age_gate);

    // Bad ids must never reach the wire.
    for (world, inst) in [
        ("usr_123", INSTANCE_ID),
        (WORLD_ID, "a/b"),
        (WORLD_ID, "a?b=c"),
        (WORLD_ID, "a..b"),
        (WORLD_ID, ""),
    ] {
        assert!(
            client.instance(world, inst).await.is_err(),
            "{world}:{inst}"
        );
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}
