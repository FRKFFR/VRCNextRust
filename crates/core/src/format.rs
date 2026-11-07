//! Pure display-formatting helpers shared by the front ends (ui-egui,
//! cli). No I/O, no egui — safe to unit-test (RULES.md §7).

use time::OffsetDateTime;
use vrc_store::{FriendEvent, FriendHistoryEntry, TimelineEntry, TimelineKind, WorldTime};

/// `Option<TimelineKind>` → display label (`None` = all kinds).
pub fn kind_label(kind: Option<TimelineKind>) -> &'static str {
    match kind {
        None => "All kinds",
        Some(TimelineKind::JoinedWorld) => "Joined world",
        Some(TimelineKind::LeftWorld) => "Left world",
        Some(TimelineKind::PlayerJoined) => "Player joined",
        Some(TimelineKind::PlayerLeft) => "Player left",
        Some(TimelineKind::PhotoTaken) => "Photo taken",
    }
}

/// [`FriendEvent`] → display label.
pub fn event_label(event: FriendEvent) -> &'static str {
    match event {
        FriendEvent::Online => "came online",
        FriendEvent::Active => "active on web/app",
        FriendEvent::Offline => "went offline",
        FriendEvent::Location => "changed location",
        FriendEvent::Added => "added as friend",
        FriendEvent::Removed => "removed friend",
        FriendEvent::Updated => "updated",
    }
}

/// `YYYY-MM-DD HH:MM` in the timestamp's own offset.
pub fn fmt_ts(ts: OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        ts.year(),
        ts.month() as u8,
        ts.day(),
        ts.hour(),
        ts.minute(),
    )
}

/// `YYYY-MM-DD HH:MM` from an ISO-8601 string (`…T…`), or the raw string
/// when it doesn't look like one. Used for REST timestamps that arrive as
/// strings (notifications, notes, moderations).
pub fn fmt_iso_ts(iso: &str) -> String {
    // ISO-8601 `2026-01-02T03:04:05…` → `2026-01-02 03:04`.
    if iso.len() >= 16 {
        let date = &iso[..10];
        let time = &iso[11..16];
        if date.len() == 10
            && time.len() == 5
            && date.as_bytes()[4] == b'-'
            && date.as_bytes()[7] == b'-'
            && iso.as_bytes()[10] == b'T'
        {
            return format!("{date} {time}");
        }
    }
    iso.to_string()
}

/// Seconds → compact `"2h 5m"` / `"45m 10s"` / `"30s"`.
pub fn format_duration(seconds: i64) -> String {
    let s = seconds.max(0);
    let hours = s / 3600;
    let minutes = (s % 3600) / 60;
    let secs = s % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {secs}s")
    } else {
        format!("{secs}s")
    }
}

/// One-line summary of a timeline row.
pub fn timeline_summary(entry: &TimelineEntry) -> String {
    match entry.kind {
        TimelineKind::JoinedWorld => {
            format!(
                "Joined {}",
                world_label_parts(&entry.world_name, &entry.world_id, &entry.location)
            )
        }
        TimelineKind::LeftWorld => "Left world".to_string(),
        TimelineKind::PlayerJoined => format!(
            "Player joined: {}",
            player_label(&entry.display_name, &entry.user_id)
        ),
        TimelineKind::PlayerLeft => format!(
            "Player left: {}",
            player_label(&entry.display_name, &entry.user_id)
        ),
        TimelineKind::PhotoTaken => format!(
            "Photo: {}",
            entry
                .detail
                .rsplit(['\\', '/'])
                .next()
                .unwrap_or(&entry.detail)
        ),
    }
}

/// One-line location summary for a friend-history row.
pub fn history_location(entry: &FriendHistoryEntry) -> String {
    if entry.location.is_empty() {
        return String::new();
    }
    // `wrld_x:instance` → `wrld_x · instance` (shorter than the raw tag).
    match entry.location.split_once(':') {
        Some((world, instance)) if world.starts_with("wrld_") => {
            format!("{world} · {instance}")
        }
        _ => entry.location.clone(),
    }
}

/// World display for stats rows: name, else id, else "a world".
pub fn world_label(world: &WorldTime) -> String {
    world_label_parts(&world.world_name, &world.world_id, "")
}

/// World display: name, else id, else location, else "a world".
fn world_label_parts(name: &str, id: &str, location: &str) -> String {
    if !name.is_empty() {
        name.to_string()
    } else if !id.is_empty() {
        id.to_string()
    } else if !location.is_empty() {
        location.to_string()
    } else {
        "a world".to_string()
    }
}

/// Sorted key/value pairs from a media row's stored PNG-metadata JSON
/// (`{}` or malformed input → empty). Used by the Media tab detail panel.
pub fn media_metadata_pairs(json: &str) -> Vec<(String, String)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(map) = value.as_object() else {
        return Vec::new();
    };
    let mut pairs: Vec<(String, String)> = map
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                v.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string()),
            )
        })
        .collect();
    pairs.sort();
    pairs
}

fn player_label(name: &str, id: &str) -> String {
    if id.is_empty() {
        name.to_string()
    } else {
        format!("{name} ({id})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
    use vrc_store::TimelineKind as Kind;

    fn ts() -> OffsetDateTime {
        PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::May, 14).unwrap(),
            Time::from_hms(20, 45, 1).unwrap(),
        )
        .assume_offset(UtcOffset::UTC)
    }

    fn timeline_entry(kind: Kind) -> TimelineEntry {
        TimelineEntry {
            id: 1,
            ts: ts(),
            kind,
            user_id: String::new(),
            display_name: String::new(),
            world_id: String::new(),
            world_name: String::new(),
            instance_id: String::new(),
            location: String::new(),
            detail: String::new(),
        }
    }

    fn history_entry(location: &str) -> FriendHistoryEntry {
        FriendHistoryEntry {
            id: 1,
            ts: ts(),
            user_id: "usr_x".into(),
            display_name: "Test Friend".into(),
            event: FriendEvent::Location,
            world_id: String::new(),
            instance_id: String::new(),
            location: location.to_string(),
            platform: String::new(),
        }
    }

    #[test]
    fn kind_labels_cover_all_variants() {
        for kind in [
            None,
            Some(Kind::JoinedWorld),
            Some(Kind::LeftWorld),
            Some(Kind::PlayerJoined),
            Some(Kind::PlayerLeft),
            Some(Kind::PhotoTaken),
        ] {
            assert!(!kind_label(kind).is_empty());
        }
        assert_eq!(kind_label(None), "All kinds");
    }

    #[test]
    fn event_labels_cover_all_variants() {
        for event in [
            FriendEvent::Online,
            FriendEvent::Active,
            FriendEvent::Offline,
            FriendEvent::Location,
            FriendEvent::Added,
            FriendEvent::Removed,
            FriendEvent::Updated,
        ] {
            assert!(!event_label(event).is_empty());
        }
    }

    #[test]
    fn fmt_ts_renders_local_fields() {
        assert_eq!(fmt_ts(ts()), "2024-05-14 20:45");
    }

    #[test]
    fn format_duration_scales() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(30), "30s");
        assert_eq!(format_duration(95), "1m 35s");
        assert_eq!(format_duration(3661), "1h 1m");
        assert_eq!(format_duration(-5), "0s");
    }

    #[test]
    fn timeline_summary_describes_each_kind() {
        let base = timeline_entry(Kind::JoinedWorld);
        assert_eq!(
            timeline_summary(&TimelineEntry {
                world_name: "Test World".into(),
                ..base.clone()
            }),
            "Joined Test World"
        );
        assert_eq!(
            timeline_summary(&TimelineEntry {
                world_name: String::new(),
                world_id: "wrld_x".into(),
                ..base.clone()
            }),
            "Joined wrld_x"
        );
        assert_eq!(
            timeline_summary(&TimelineEntry {
                kind: Kind::PlayerJoined,
                display_name: "Test User".into(),
                user_id: "usr_x".into(),
                ..base.clone()
            }),
            "Player joined: Test User (usr_x)"
        );
        assert_eq!(
            timeline_summary(&TimelineEntry {
                kind: Kind::PhotoTaken,
                detail: "C:\\pics\\a.png".into(),
                ..base.clone()
            }),
            "Photo: a.png"
        );
        assert_eq!(
            timeline_summary(&TimelineEntry {
                kind: Kind::LeftWorld,
                ..base
            }),
            "Left world"
        );
    }

    #[test]
    fn history_location_splits_wrld_tags() {
        assert_eq!(
            history_location(&history_entry("wrld_x:55~region(us)")),
            "wrld_x · 55~region(us)"
        );
        assert_eq!(history_location(&history_entry("private")), "private");
        assert!(history_location(&history_entry("")).is_empty());
    }

    #[test]
    fn world_label_prefers_name() {
        let world = WorldTime {
            world_id: "wrld_x".into(),
            world_name: "Name".into(),
            ..WorldTime::default()
        };
        assert_eq!(world_label(&world), "Name");
        let world = WorldTime {
            world_id: "wrld_x".into(),
            ..WorldTime::default()
        };
        assert_eq!(world_label(&world), "wrld_x");
        assert_eq!(world_label(&WorldTime::default()), "a world");
    }
}
