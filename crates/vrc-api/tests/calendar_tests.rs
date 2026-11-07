//! M7 calendar endpoint tests against `wiremock` with synthetic fixtures
//! only (RULES.md §6). Paths/params follow the community-documented
//! calendar API; invalid ids must be rejected before any request.

use serde_json::json;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use vrc_api::{
    CalendarEventPage, Client, DiscoverParams, DiscoverScope, GroupCalendarParams, MemoryVault,
    ResultFilter,
};

const GROUP_ID: &str = "grp_00000000-0000-0000-0000-000000000001";
const EVENT_ID: &str = "cal_00000000-0000-0000-0000-000000000001";

fn test_client(server: &MockServer) -> Client {
    Client::builder()
        .base_url(server.uri())
        .vault(MemoryVault::new())
        .rate_limit(1_000.0, 100)
        .build()
        .expect("client builds")
}

fn fake_event() -> serde_json::Value {
    json!({
        "id": EVENT_ID,
        "seriesId": EVENT_ID,
        "ownerId": GROUP_ID,
        "title": "Synthetic Event",
        "description": "Fixture event.",
        "category": "social",
        "accessType": "group",
        "type": "event",
        "startsAt": "2026-02-01T20:00:00Z",
        "endsAt": "2026-02-01T21:00:00Z",
        "durationInMs": 3600000,
        "occurrenceKind": "single",
        "occurrenceModified": false,
        "imageId": "file_00000000-0000-0000-0000-000000000002",
        "imageUrl": "",
        "interestedUserCount": 3,
        "isDraft": false,
        "featured": false,
        "languages": ["eng"],
        "platforms": ["standalonewindows"],
        "roleIds": [],
        "tags": ["fixture"],
        "hostEarlyJoinMinutes": 0,
        "guestEarlyJoinMinutes": 0,
        "closeInstanceAfterEndMinutes": 0,
        "usesInstanceOverflow": false,
        "userInterest": {"createdAt": "2026-01-01T00:00:00Z", "isFollowing": false, "updatedAt": "2026-01-01T00:00:00Z"},
        "createdAt": "2026-01-01T00:00:00Z",
        "updatedAt": "2026-01-01T00:00:00Z",
        "deletedAt": null
    })
}

fn page(events: Vec<serde_json::Value>) -> serde_json::Value {
    json!({"hasNext": false, "results": events, "totalCount": events.len()})
}

#[tokio::test]
async fn calendar_events_sends_documented_params() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/calendar"))
        .and(query_param("date", "2026-02"))
        .and(query_param("n", "30"))
        .and(query_param("offset", "60"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![fake_event()])))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let page: CalendarEventPage = client
        .calendar_events(Some("2026-02"), Some(30), Some(60))
        .await
        .unwrap();
    assert_eq!(page.results.len(), 1);
    assert_eq!(page.results[0].title, "Synthetic Event");
}

#[tokio::test]
async fn calendar_discover_sends_all_params() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/calendar/discover"))
        .and(query_param("scope", "upcoming"))
        .and(query_param("categories", "social,performance"))
        .and(query_param("tags", "fixture"))
        .and(query_param("featuredResults", "include"))
        .and(query_param("nonFeaturedResults", "skip"))
        .and(query_param("personalizedResults", "exclude"))
        .and(query_param("minimumInterestCount", "5"))
        .and(query_param("minimumRemainingMinutes", "15"))
        .and(query_param("upcomingOffsetMinutes", "60"))
        .and(query_param("n", "25"))
        .and(query_param("nextCursor", "cursor-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "nextCursor": "cursor-2",
            "results": [fake_event()]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let params = DiscoverParams {
        scope: Some(DiscoverScope::Upcoming),
        categories: &["social", "performance"],
        tags: &["fixture"],
        featured_results: Some(ResultFilter::Include),
        non_featured_results: Some(ResultFilter::Skip),
        personalized_results: Some(ResultFilter::Exclude),
        minimum_interest_count: Some(5),
        minimum_remaining_minutes: Some(15),
        upcoming_offset_minutes: Some(60),
        n: Some(25),
        next_cursor: Some("cursor-1"),
    };
    let disc = client.calendar_discover(&params).await.unwrap();
    assert_eq!(disc.next_cursor.as_deref(), Some("cursor-2"));
    assert_eq!(disc.results.len(), 1);
}

#[tokio::test]
async fn calendar_discover_defaults_scope_and_n() {
    let server = MockServer::start().await;
    // No params at all — documented defaults apply server-side; we only
    // send `n` (clamped) unless the caller overrides scope.
    Mock::given(method("GET"))
        .and(path("/calendar/discover"))
        .and(query_param("n", "60"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let disc = client
        .calendar_discover(&DiscoverParams::default())
        .await
        .unwrap();
    assert!(disc.results.is_empty());
    assert!(disc.next_cursor.is_none());
}

#[tokio::test]
async fn calendar_featured_and_search() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/calendar/featured"))
        .and(query_param("date", "2026-03"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/calendar/search"))
        .and(query_param("searchTerm", "january meet"))
        .and(query_param("utcOffset", "2"))
        .and(query_param("n", "10"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![fake_event()])))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    client
        .calendar_featured(Some("2026-03"), None, None)
        .await
        .unwrap();
    let found = client
        .calendar_search("january meet", Some(2), Some(10), None)
        .await
        .unwrap();
    assert_eq!(found.results.len(), 1);
}

#[tokio::test]
async fn group_calendar_endpoints() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/calendar/{GROUP_ID}")))
        .and(query_param("date", "2026-02"))
        .and(query_param("after", "2026-02-01T00:00:00Z"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![fake_event()])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/calendar/{GROUP_ID}/{EVENT_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_event()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/calendar/{GROUP_ID}/next")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_event()))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let events = client
        .group_calendar_events(
            GROUP_ID,
            &GroupCalendarParams {
                date: Some("2026-02"),
                after: Some("2026-02-01T00:00:00Z"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(events.results.len(), 1);

    let event = client
        .group_calendar_event(GROUP_ID, EVENT_ID)
        .await
        .unwrap();
    assert_eq!(event.id, EVENT_ID);

    let next = client.group_next_calendar_event(GROUP_ID).await.unwrap();
    assert_eq!(next.id, EVENT_ID);
}

#[tokio::test]
async fn follow_calendar_event_posts_is_following() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/calendar/{GROUP_ID}/{EVENT_ID}/follow")))
        .and(body_json(json!({"isFollowing": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_event()))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let event = client
        .follow_calendar_event(GROUP_ID, EVENT_ID, true)
        .await
        .unwrap();
    assert_eq!(event.id, EVENT_ID);
}

#[tokio::test]
async fn invalid_ids_rejected_before_request() {
    let server = MockServer::start().await;
    // Exactly one request may reach the wire: the valid group call below.
    // Every invalid id must be rejected client-side first.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(vec![])))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    assert!(client
        .group_calendar_events("not-a-group", &GroupCalendarParams::default())
        .await
        .is_err());
    assert!(client
        .group_calendar_events(GROUP_ID, &GroupCalendarParams::default())
        .await
        .is_ok());
    assert!(client
        .group_calendar_event(GROUP_ID, "cal_bad/id")
        .await
        .is_err());
    assert!(client.group_next_calendar_event("usr_123").await.is_err());
    assert!(client
        .follow_calendar_event(GROUP_ID, "bad", true)
        .await
        .is_err());
    assert!(client
        .calendar_search("   ", None, None, None)
        .await
        .is_err());
}
