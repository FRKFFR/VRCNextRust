//! Calendar endpoints (M7): discover, user/featured/search lists, and
//! per-group event lists — all read-only, per the community-documented
//! API (vrchat.community reference).
//!
//! Every path id is validated before use; every call travels through
//! [`Client::send`] — rate limit, retry, 401 handling (RULES.md §3, §4).

use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::groups::valid_group_id;
use crate::models::null_to_default;
use crate::query::{append_page, append_query, append_query_opt, MAX_PAGE_SIZE};
use crate::{ApiError, Client};

fn check_prefixed_id(prefix: &str, id: &str) -> bool {
    id.strip_prefix(prefix).is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    })
}

/// `true` for a well-formed `cal_…` calendar-event id.
pub fn valid_calendar_event_id(id: &str) -> bool {
    check_prefixed_id("cal_", id)
}

/// `true` for a well-formed `file_…` image/file id (calendar event art).
pub fn valid_file_id(id: &str) -> bool {
    check_prefixed_id("file_", id)
}

fn check_group_id(id: &str) -> Result<(), ApiError> {
    if valid_group_id(id) {
        Ok(())
    } else {
        Err(ApiError::InvalidId(id.chars().take(64).collect()))
    }
}

fn check_event_id(id: &str) -> Result<(), ApiError> {
    if valid_calendar_event_id(id) {
        Ok(())
    } else {
        Err(ApiError::InvalidId(id.chars().take(64).collect()))
    }
}

/// Documented recurrence `end.type` values are open-ended strings; the
/// whole `recurrence` object is modelled loosely so new fields never
/// break decoding.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CalendarRecurrenceEnd {
    /// Occurrence count limit (`afterOccurrences` ends).
    #[serde(deserialize_with = "null_to_default")]
    pub count: u64,
    /// End date (`YYYY-MM-DD` or date-time).
    #[serde(deserialize_with = "null_to_default")]
    pub date: String,
    /// `type` on the wire (`afterOccurrences`, …).
    #[serde(rename = "type", deserialize_with = "null_to_default")]
    pub kind: String,
}

/// Documented `recurrence` object on a [`CalendarEvent`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CalendarRecurrence {
    /// Weekdays the event repeats on (`MO`, `TU`, …).
    #[serde(deserialize_with = "null_to_default")]
    pub days_of_week: Vec<String>,
    /// Recurrence end condition.
    pub end: Option<CalendarRecurrenceEnd>,
    /// `daily` | `weekly` | `monthly` | `yearly`.
    #[serde(deserialize_with = "null_to_default")]
    pub frequency: String,
    /// Repeat every N units.
    #[serde(deserialize_with = "null_to_default")]
    pub interval: u32,
    /// IANA timezone name.
    #[serde(deserialize_with = "null_to_default")]
    pub timezone: String,
}

/// Documented `userInterest` object — the requesting user's follow state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CalendarUserInterest {
    /// When the interest was created (ISO-8601).
    #[serde(deserialize_with = "null_to_default")]
    pub created_at: String,
    /// Whether the user follows the event.
    #[serde(rename = "isFollowing", deserialize_with = "null_to_default")]
    pub is_following: bool,
    /// When the interest was last updated (ISO-8601).
    #[serde(deserialize_with = "null_to_default")]
    pub updated_at: String,
}

/// A calendar event — the documented schema shared by every calendar
/// endpoint. All fields default; unknown keys are ignored.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CalendarEvent {
    /// `cal_…` event id.
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    /// Recurring-series id (`cal_…`); equals `id` for single events.
    #[serde(deserialize_with = "null_to_default")]
    pub series_id: String,
    /// Owning `grp_…`/`usr_…` id.
    #[serde(deserialize_with = "null_to_default")]
    pub owner_id: String,
    /// Event title.
    #[serde(deserialize_with = "null_to_default")]
    pub title: String,
    /// Event description (may contain markup).
    #[serde(deserialize_with = "null_to_default")]
    pub description: String,
    /// Event category (`performance`, `social`, …).
    #[serde(deserialize_with = "null_to_default")]
    pub category: String,
    /// `accessType` on the wire (`group`, …).
    #[serde(rename = "accessType", deserialize_with = "null_to_default")]
    pub access_type: String,
    /// `type` on the wire (`event`, …).
    #[serde(rename = "type", deserialize_with = "null_to_default")]
    pub event_type: String,
    /// Start time (ISO-8601).
    #[serde(rename = "startsAt", deserialize_with = "null_to_default")]
    pub starts_at: String,
    /// End time (ISO-8601).
    #[serde(rename = "endsAt", deserialize_with = "null_to_default")]
    pub ends_at: String,
    /// Duration in milliseconds.
    #[serde(rename = "durationInMs", deserialize_with = "null_to_default")]
    pub duration_in_ms: i64,
    /// `single` | `occurrence` | …
    #[serde(rename = "occurrenceKind", deserialize_with = "null_to_default")]
    pub occurrence_kind: String,
    /// Whether this occurrence was modified from the series.
    #[serde(rename = "occurrenceModified", deserialize_with = "null_to_default")]
    pub occurrence_modified: bool,
    /// Recurrence rule for series events.
    pub recurrence: Option<CalendarRecurrence>,
    /// `file_…` image id.
    #[serde(rename = "imageId", deserialize_with = "null_to_default")]
    pub image_id: String,
    /// Image URL (may be empty).
    #[serde(rename = "imageUrl", deserialize_with = "null_to_default")]
    pub image_url: String,
    /// Interested/follower count.
    #[serde(rename = "interestedUserCount", deserialize_with = "null_to_default")]
    pub interested_user_count: u64,
    /// Whether the event is a draft.
    #[serde(rename = "isDraft", deserialize_with = "null_to_default")]
    pub is_draft: bool,
    /// Featured flag.
    #[serde(deserialize_with = "null_to_default")]
    pub featured: bool,
    /// ISO-639 language codes.
    #[serde(deserialize_with = "null_to_default")]
    pub languages: Vec<String>,
    /// Supported platforms (`android`, `ios`, `standalonewindows`, …).
    #[serde(deserialize_with = "null_to_default")]
    pub platforms: Vec<String>,
    /// `grol_…` role ids required to attend.
    #[serde(rename = "roleIds", deserialize_with = "null_to_default")]
    pub role_ids: Vec<String>,
    /// Tags.
    #[serde(deserialize_with = "null_to_default")]
    pub tags: Vec<String>,
    /// Minutes the host may join early.
    #[serde(rename = "hostEarlyJoinMinutes", deserialize_with = "null_to_default")]
    pub host_early_join_minutes: i64,
    /// Minutes guests may join early.
    #[serde(rename = "guestEarlyJoinMinutes", deserialize_with = "null_to_default")]
    pub guest_early_join_minutes: i64,
    /// Minutes after end the instance stays open.
    #[serde(
        rename = "closeInstanceAfterEndMinutes",
        deserialize_with = "null_to_default"
    )]
    pub close_instance_after_end_minutes: i64,
    /// Whether the event uses instance overflow.
    #[serde(rename = "usesInstanceOverflow", deserialize_with = "null_to_default")]
    pub uses_instance_overflow: bool,
    /// Requesting user's follow state.
    #[serde(rename = "userInterest")]
    pub user_interest: Option<CalendarUserInterest>,
    /// Creation time (ISO-8601).
    #[serde(rename = "createdAt", deserialize_with = "null_to_default")]
    pub created_at: String,
    /// Last update time (ISO-8601).
    #[serde(rename = "updatedAt", deserialize_with = "null_to_default")]
    pub updated_at: String,
    /// Deletion time (ISO-8601); set for soft-deleted events.
    #[serde(rename = "deletedAt", deserialize_with = "null_to_default")]
    pub deleted_at: String,
}

/// Paged calendar list (`GET /calendar`, `/calendar/featured`,
/// `/calendar/{groupId}`, `/calendar/search`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CalendarEventPage {
    /// Whether another page exists.
    #[serde(rename = "hasNext", deserialize_with = "null_to_default")]
    pub has_next: bool,
    /// Events on this page.
    #[serde(deserialize_with = "null_to_default")]
    pub results: Vec<CalendarEvent>,
    /// Total matching events.
    #[serde(rename = "totalCount", deserialize_with = "null_to_default")]
    pub total_count: u64,
}

/// `GET /calendar/discover` response: cursor-paginated discovery results.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CalendarEventDiscovery {
    /// Cursor for the next page; absent/empty when exhausted.
    #[serde(rename = "nextCursor")]
    pub next_cursor: Option<String>,
    /// Discovered events.
    #[serde(deserialize_with = "null_to_default")]
    pub results: Vec<CalendarEvent>,
}

/// Documented `scope` values for `GET /calendar/discover`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiscoverScope {
    /// All events regardless of time.
    All,
    /// Currently-live events.
    Live,
    /// Upcoming events (default).
    #[default]
    Upcoming,
}

impl DiscoverScope {
    /// The documented wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Live => "live",
            Self::Upcoming => "upcoming",
        }
    }
}

/// Documented include/exclude filter values for discovery result groups.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ResultFilter {
    /// Omit this group entirely.
    Exclude,
    /// Include this group (default).
    #[default]
    Include,
    /// Skip ranking/boosting for this group.
    Skip,
}

impl ResultFilter {
    /// The documented wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exclude => "exclude",
            Self::Include => "include",
            Self::Skip => "skip",
        }
    }
}

/// Query parameters for `GET /calendar/discover`. All fields optional;
/// defaults match the documented API defaults.
#[derive(Debug, Clone, Default)]
pub struct DiscoverParams<'a> {
    /// Discovery scope (`upcoming` default).
    pub scope: Option<DiscoverScope>,
    /// Category filter (comma-separated on the wire).
    pub categories: &'a [&'a str],
    /// Tag filter (comma-separated on the wire).
    pub tags: &'a [&'a str],
    /// Featured-results filter.
    pub featured_results: Option<ResultFilter>,
    /// Non-featured-results filter.
    pub non_featured_results: Option<ResultFilter>,
    /// Personalized-results filter.
    pub personalized_results: Option<ResultFilter>,
    /// Minimum interested-user count.
    pub minimum_interest_count: Option<u32>,
    /// Minimum minutes remaining before start.
    pub minimum_remaining_minutes: Option<u32>,
    /// Offset in minutes for the upcoming window.
    pub upcoming_offset_minutes: Option<i64>,
    /// Page size (1–100, default 60).
    pub n: Option<u32>,
    /// Cursor from a previous discovery response.
    pub next_cursor: Option<&'a str>,
}

/// Query parameters for `GET /calendar/{groupId}`.
#[derive(Debug, Clone, Default)]
pub struct GroupCalendarParams<'a> {
    /// Month to list.
    pub date: Option<&'a str>,
    /// Page size (1–100).
    pub n: Option<u32>,
    /// Zero-based offset.
    pub offset: Option<u64>,
    /// Maximum entries.
    pub limit: Option<u32>,
    /// Only events starting after this date-time.
    pub after: Option<&'a str>,
    /// Sort order (documented as free-form string).
    pub sort: Option<&'a str>,
}

fn append_int(path: &mut String, key: &str, value: Option<i64>) {
    if let Some(v) = value {
        append_query(path, key, &v.to_string());
    }
}

fn append_u32(path: &mut String, key: &str, value: Option<u32>) {
    if let Some(v) = value {
        append_query(path, key, &v.to_string());
    }
}

fn append_u64(path: &mut String, key: &str, value: Option<u64>) {
    if let Some(v) = value {
        append_query(path, key, &v.to_string());
    }
}

fn append_csv(path: &mut String, key: &str, values: &[&str]) {
    if !values.is_empty() {
        append_query(path, key, &values.join(","));
    }
}

impl Client {
    /// `GET /calendar` — the user's calendar events for the month in
    /// `date` (documented `date-time` format).
    pub async fn calendar_events(
        &self,
        date: Option<&str>,
        n: Option<u32>,
        offset: Option<u64>,
    ) -> Result<CalendarEventPage, ApiError> {
        let mut path = String::from("/calendar");
        append_query_opt(&mut path, "date", date);
        append_page(&mut path, n, offset);
        self.get_json(&path).await
    }

    /// `GET /calendar/discover` — cursor-paginated event discovery.
    /// First call uses no `nextCursor`; subsequent calls pass the
    /// `next_cursor` from the previous response.
    pub async fn calendar_discover(
        &self,
        params: &DiscoverParams<'_>,
    ) -> Result<CalendarEventDiscovery, ApiError> {
        let mut path = String::from("/calendar/discover");
        if let Some(scope) = params.scope {
            append_query(&mut path, "scope", scope.as_str());
        }
        append_csv(&mut path, "categories", params.categories);
        append_csv(&mut path, "tags", params.tags);
        if let Some(f) = params.featured_results {
            append_query(&mut path, "featuredResults", f.as_str());
        }
        if let Some(f) = params.non_featured_results {
            append_query(&mut path, "nonFeaturedResults", f.as_str());
        }
        if let Some(f) = params.personalized_results {
            append_query(&mut path, "personalizedResults", f.as_str());
        }
        append_u32(
            &mut path,
            "minimumInterestCount",
            params.minimum_interest_count,
        );
        append_u32(
            &mut path,
            "minimumRemainingMinutes",
            params.minimum_remaining_minutes,
        );
        append_int(
            &mut path,
            "upcomingOffsetMinutes",
            params.upcoming_offset_minutes,
        );
        // `n` is clamped to the documented 1..=100 range (default 60).
        let n = params.n.unwrap_or(60).clamp(1, MAX_PAGE_SIZE);
        append_query(&mut path, "n", &n.to_string());
        append_query_opt(&mut path, "nextCursor", params.next_cursor);
        self.get_json(&path).await
    }

    /// `GET /calendar/featured` — featured events for the month in `date`.
    pub async fn calendar_featured(
        &self,
        date: Option<&str>,
        n: Option<u32>,
        offset: Option<u64>,
    ) -> Result<CalendarEventPage, ApiError> {
        let mut path = String::from("/calendar/featured");
        append_query_opt(&mut path, "date", date);
        append_page(&mut path, n, offset);
        self.get_json(&path).await
    }

    /// `GET /calendar/search` — events matching `search_term`.
    /// `utc_offset` is clamped to the documented -12..=12 hours.
    pub async fn calendar_search(
        &self,
        search_term: &str,
        utc_offset: Option<i64>,
        n: Option<u32>,
        offset: Option<u64>,
    ) -> Result<CalendarEventPage, ApiError> {
        if search_term.trim().is_empty() {
            return Err(ApiError::InvalidId("empty search term".to_string()));
        }
        let mut path = String::from("/calendar/search");
        append_query(&mut path, "searchTerm", search_term);
        if let Some(off) = utc_offset {
            append_query(&mut path, "utcOffset", &off.clamp(-12, 12).to_string());
        }
        append_page(&mut path, n, offset);
        self.get_json(&path).await
    }

    /// `GET /calendar/{groupId}` — a group's calendar events.
    pub async fn group_calendar_events(
        &self,
        group_id: &str,
        params: &GroupCalendarParams<'_>,
    ) -> Result<CalendarEventPage, ApiError> {
        check_group_id(group_id)?;
        let mut path = format!("/calendar/{group_id}");
        append_query_opt(&mut path, "date", params.date);
        append_u32(&mut path, "n", params.n);
        append_u64(&mut path, "offset", params.offset);
        append_u32(&mut path, "limit", params.limit);
        append_query_opt(&mut path, "after", params.after);
        append_query_opt(&mut path, "sort", params.sort);
        self.get_json(&path).await
    }

    /// `GET /calendar/{groupId}/{calendarId}` — one calendar event.
    /// `calendar_id` is the event's `cal_…` id.
    pub async fn group_calendar_event(
        &self,
        group_id: &str,
        calendar_id: &str,
    ) -> Result<CalendarEvent, ApiError> {
        check_group_id(group_id)?;
        check_event_id(calendar_id)?;
        self.get_json(&format!("/calendar/{group_id}/{calendar_id}"))
            .await
    }

    /// `GET /calendar/{groupId}/next` — the group's next upcoming event.
    /// The API answers 404 when the group has no upcoming events.
    pub async fn group_next_calendar_event(
        &self,
        group_id: &str,
    ) -> Result<CalendarEvent, ApiError> {
        check_group_id(group_id)?;
        self.get_json(&format!("/calendar/{group_id}/next")).await
    }

    /// `POST /calendar/{groupId}/{calendarId}/follow` — follow/unfollow an
    /// event on a group's calendar. Documented body: `{"isFollowing": bool}`.
    pub async fn follow_calendar_event(
        &self,
        group_id: &str,
        calendar_id: &str,
        is_following: bool,
    ) -> Result<CalendarEvent, ApiError> {
        check_group_id(group_id)?;
        check_event_id(calendar_id)?;
        let body = serde_json::json!({ "isFollowing": is_following });
        self.send_json(
            Method::POST,
            &format!("/calendar/{group_id}/{calendar_id}/follow"),
            Some(&body),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_validation() {
        assert!(valid_calendar_event_id(
            "cal_6b182f0c-61ef-4bdf-97fe-94f63bcba27b"
        ));
        assert!(valid_file_id("file_ce35d830-e20a-4df0-a6d4-5aaef4508044"));
        for bad in ["cal_", "usr_123", "cal_a/b", "cal_a?b", "cal_a b", ""] {
            assert!(!valid_calendar_event_id(bad), "{bad}");
        }
    }

    #[test]
    fn event_decodes_documented_shape() {
        // Synthetic fixture written from the documented schema — fake ids.
        let json = r#"{
            "accessType": "group",
            "category": "performance",
            "closeInstanceAfterEndMinutes": 30,
            "createdAt": "2026-01-01T00:00:00Z",
            "deletedAt": null,
            "description": "Test event",
            "durationInMs": 3600000,
            "endsAt": "2026-01-01T02:00:00Z",
            "featured": true,
            "guestEarlyJoinMinutes": 5,
            "hostEarlyJoinMinutes": 10,
            "id": "cal_00000000-0000-0000-0000-000000000001",
            "imageId": "file_00000000-0000-0000-0000-000000000002",
            "imageUrl": "",
            "interestedUserCount": 42,
            "isDraft": false,
            "languages": ["eng"],
            "occurrenceKind": "single",
            "occurrenceModified": false,
            "ownerId": "grp_00000000-0000-0000-0000-000000000003",
            "platforms": ["standalonewindows"],
            "recurrence": null,
            "roleIds": [],
            "seriesId": "cal_00000000-0000-0000-0000-000000000001",
            "startsAt": "2026-01-01T01:00:00Z",
            "tags": ["test"],
            "title": "Test Event",
            "type": "event",
            "updatedAt": "2026-01-01T00:30:00Z",
            "userInterest": {"createdAt": "2026-01-01T00:00:00Z", "isFollowing": true, "updatedAt": "2026-01-01T00:00:00Z"},
            "usesInstanceOverflow": false
        }"#;
        let ev: CalendarEvent = serde_json::from_str(json).unwrap();
        assert_eq!(ev.id, "cal_00000000-0000-0000-0000-000000000001");
        assert_eq!(ev.title, "Test Event");
        assert!(ev.featured);
        assert!(ev.user_interest.unwrap().is_following);
        assert!(ev.recurrence.is_none());
    }

    #[test]
    fn page_and_discovery_decode() {
        let page: CalendarEventPage =
            serde_json::from_str(r#"{"hasNext": true, "results": [], "totalCount": 7}"#).unwrap();
        assert!(page.has_next);
        assert_eq!(page.total_count, 7);

        let disc: CalendarEventDiscovery =
            serde_json::from_str(r#"{"nextCursor": "abc", "results": []}"#).unwrap();
        assert_eq!(disc.next_cursor.as_deref(), Some("abc"));

        let disc: CalendarEventDiscovery = serde_json::from_str(r#"{"results": []}"#).unwrap();
        assert!(disc.next_cursor.is_none());
    }
}
