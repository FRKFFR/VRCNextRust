//! `GET /worlds/{id}` tests against `wiremock` with synthetic fixtures
//! only (RULES.md §6). No live network.

use serde_json::json;
use wiremock::matchers::{method, path};
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

#[tokio::test]
async fn world_decodes_public_metadata() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/worlds/wrld_00000000-0000-0000-0000-00000000000a",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "wrld_00000000-0000-0000-0000-00000000000a",
            "name": "Test World",
            "description": "Synthetic test fixture, not a real world.",
            "authorName": "Test Author",
            "authorId": "usr_00000000-0000-0000-0000-000000000099",
            "capacity": 40,
            "recommendedCapacity": 32,
            "imageUrl": "https://api.vrchat.cloud/api/1/file/file_00000000-0000-0000-0000-000000000002/1/file",
            "thumbnailImageUrl": "https://api.vrchat.cloud/api/1/image/file_00000000-0000-0000-0000-000000000002/1/256",
            "url": "wrld_00000000-0000-0000-0000-00000000000a:1",
            "popularity": 100,
            "favorites": 5,
            "occupancy": 3,
            "releaseStatus": "public",
            "platforms": ["standalonewindows", "quest"],
            "createdAt": "2026-01-02T03:04:05.000Z",
            "updatedAt": "2026-01-03T03:04:05.000Z",
            "publicationDate": "2026-01-04T03:04:05.000Z"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let world = test_client(&server)
        .world("wrld_00000000-0000-0000-0000-00000000000a")
        .await
        .expect("world fetch");
    assert_eq!(world.name, "Test World");
    assert_eq!(world.author_name, "Test Author");
    assert_eq!(world.capacity, 40);
    assert_eq!(world.platforms, ["standalonewindows", "quest"]);
    assert!(world.thumbnail_image_url.ends_with("/256"));
    server.verify().await;
}

#[tokio::test]
async fn world_tolerates_null_fields() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/worlds/wrld_00000000-0000-0000-0000-00000000000b"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "wrld_00000000-0000-0000-0000-00000000000b",
            "name": null,
            "description": null,
            "authorName": null,
            "capacity": null,
            "platforms": null,
            "favorites": null
        })))
        .mount(&server)
        .await;

    let world = test_client(&server)
        .world("wrld_00000000-0000-0000-0000-00000000000b")
        .await
        .expect("world fetch");
    assert!(world.name.is_empty());
    assert_eq!(world.capacity, 0);
    assert!(world.platforms.is_empty());
}

#[tokio::test]
async fn world_404_is_an_http_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/worlds/wrld_00000000-0000-0000-0000-00000000000c"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": {"message": "synthetic not found", "status_code": 404}
        })))
        .mount(&server)
        .await;

    let err = test_client(&server)
        .world("wrld_00000000-0000-0000-0000-00000000000c")
        .await
        .expect_err("world fetch must fail");
    assert!(err.to_string().contains("404"));
}
