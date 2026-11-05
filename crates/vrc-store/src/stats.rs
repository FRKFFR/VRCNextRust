//! Time-spent statistics over sessions and friend presence history.
//!
//! World time comes straight from `sessions` rows clipped to the requested
//! range. Friend time ("time together") is reconstructed from
//! `friend_history`: each friend's known location forms presence intervals,
//! and the seconds those intervals overlap *my* sessions in the same
//! world instance are credited to that friend.

use std::collections::HashMap;

use time::OffsetDateTime;

use crate::history::{rows_up_to, FriendEvent};
use crate::sessions::sessions_in_range;
use crate::{lock, ts_to_i64, Conn, StoreError};

/// Optional time bounds for [`StatsRepo::compute`]; `None` = unbounded.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatsRange {
    /// Earliest instant included.
    pub since: Option<OffsetDateTime>,
    /// Latest instant included (exclusive).
    pub until: Option<OffsetDateTime>,
}

/// Time spent in one world.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorldTime {
    /// `wrld_…` id; empty for sessions whose location never resolved.
    pub world_id: String,
    /// Latest known world display name; empty when unknown.
    pub world_name: String,
    /// Seconds spent in the world inside the range.
    pub seconds: i64,
    /// Number of sessions in the world inside the range.
    pub sessions: u64,
}

/// Time spent together with one friend (same world instance).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FriendTime {
    /// `usr_…` friend id.
    pub user_id: String,
    /// Latest known display name; the user id when never named.
    pub display_name: String,
    /// Seconds of overlapping session/presence time.
    pub seconds: i64,
}

/// Aggregated statistics for a [`StatsRange`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    /// Total seconds spent in worlds inside the range.
    pub total_seconds: i64,
    /// Number of sessions inside the range.
    pub session_count: u64,
    /// Top worlds by seconds, descending.
    pub top_worlds: Vec<WorldTime>,
    /// Top friends by seconds together, descending.
    pub top_friends: Vec<FriendTime>,
}

/// Repository for aggregated statistics.
#[derive(Clone)]
pub struct StatsRepo {
    pub(crate) conn: Conn,
}

/// `(world_id, instance_number)` — the granularity at which a friend's
/// location is compared with my sessions. The instance number is the part
/// of the instance spec before the first `~` (`1234` in
/// `1234~hidden(usr_x)~region(jp)`).
type InstanceKey = (String, String);

fn instance_num(instance_id: &str) -> &str {
    instance_id.split('~').next().unwrap_or("")
}

/// Session/friend location key: `world_id` + instance number.
fn key_of(world_id: &str, instance_id: &str) -> InstanceKey {
    (world_id.to_string(), instance_num(instance_id).to_string())
}

/// `wrld_…` id from a `wrld_…:instance` location string, or "".
fn world_from_location(location: &str) -> &str {
    if location.starts_with("wrld_") {
        location.split(':').next().unwrap_or("")
    } else {
        ""
    }
}

/// Instance spec from a `wrld_…:instance` location string (everything
/// after `wrld_…:`), or "".
fn instance_from_location(location: &str) -> &str {
    if location.starts_with("wrld_") {
        location.split_once(':').map(|(_, i)| i).unwrap_or("")
    } else {
        ""
    }
}

impl StatsRepo {
    pub(crate) fn new(conn: Conn) -> Self {
        Self { conn }
    }

    /// Compute statistics for `range`; `top` caps each top-N list
    /// (`0` = no cap).
    pub fn compute(&self, range: &StatsRange, top: usize) -> Result<Stats, StoreError> {
        let conn = lock(&self.conn);
        let since = range.since.map(ts_to_i64);
        let until = range.until.map(ts_to_i64);
        // Open sessions count until `now`, clipped by `until` below.
        let now = OffsetDateTime::now_utc();
        let now_i64 = ts_to_i64(now);
        let range_end = until.unwrap_or(now_i64);
        let range_start = since.unwrap_or(i64::MIN);

        // --- world time -------------------------------------------------
        let sessions = sessions_in_range(&conn, since, until)?;
        let mut total_seconds = 0i64;
        let mut session_count = 0u64;
        let mut worlds: HashMap<String, WorldTime> = HashMap::new();
        // Instance-keyed session intervals for the friend overlap pass.
        let mut session_intervals: HashMap<InstanceKey, Vec<(i64, i64)>> = HashMap::new();

        for session in &sessions {
            let start = ts_to_i64(session.started_at).max(range_start);
            let end = session
                .ended_at
                .map(ts_to_i64)
                .unwrap_or(now_i64)
                .min(range_end);
            if end <= start {
                continue;
            }
            let seconds = end - start;
            total_seconds += seconds;
            session_count += 1;
            let world = worlds
                .entry(session.world_id.clone())
                .or_insert_with(|| WorldTime {
                    world_id: session.world_id.clone(),
                    ..WorldTime::default()
                });
            world.seconds += seconds;
            world.sessions += 1;
            if !session.world_name.is_empty() {
                world.world_name = session.world_name.clone();
            }
            if !session.world_id.is_empty() {
                session_intervals
                    .entry(key_of(&session.world_id, &session.instance_id))
                    .or_default()
                    .push((start, end));
            }
        }

        // --- friend time together ---------------------------------------
        let friend_rows = rows_up_to(&conn, until.unwrap_or(i64::MAX))?;
        let mut friend_seconds: HashMap<String, i64> = HashMap::new();
        let mut friend_names: HashMap<String, String> = HashMap::new();

        let mut iter = friend_rows.into_iter().peekable();
        while let Some(first) = iter.next() {
            let user_id = first.user_id.clone();
            // Rows are grouped by user_id; gather this user's run.
            let mut rows = vec![first];
            while let Some(row) = iter.next_if(|r| r.user_id == user_id) {
                rows.push(row);
            }
            accumulate_friend(
                &rows,
                range_start,
                range_end,
                &session_intervals,
                &mut friend_seconds,
            );
            if let Some(name) = rows
                .iter()
                .rev()
                .map(|r| r.display_name.as_str())
                .find(|n| !n.is_empty())
            {
                friend_names.insert(user_id, name.to_string());
            }
        }

        let mut top_worlds: Vec<WorldTime> = worlds.into_values().collect();
        top_worlds.sort_by(|a, b| {
            b.seconds
                .cmp(&a.seconds)
                .then_with(|| a.world_id.cmp(&b.world_id))
        });
        if top > 0 {
            top_worlds.truncate(top);
        }

        let mut top_friends: Vec<FriendTime> = friend_seconds
            .into_iter()
            .filter(|(_, seconds)| *seconds > 0)
            .map(|(user_id, seconds)| FriendTime {
                display_name: friend_names
                    .get(&user_id)
                    .cloned()
                    .unwrap_or_else(|| user_id.clone()),
                user_id,
                seconds,
            })
            .collect();
        top_friends.sort_by(|a, b| {
            b.seconds
                .cmp(&a.seconds)
                .then_with(|| a.user_id.cmp(&b.user_id))
        });
        if top > 0 {
            top_friends.truncate(top);
        }

        Ok(Stats {
            total_seconds,
            session_count,
            top_worlds,
            top_friends,
        })
    }
}

/// Reconstruct one friend's presence intervals from their history rows and
/// credit overlaps with same-instance session intervals.
fn accumulate_friend(
    rows: &[crate::FriendHistoryEntry],
    range_start: i64,
    range_end: i64,
    session_intervals: &HashMap<InstanceKey, Vec<(i64, i64)>>,
    friend_seconds: &mut HashMap<String, i64>,
) {
    // Current presence: (key, segment start ts). A `None` key means the
    // friend is not in a world instance (offline, web-active,
    // private/traveling, unknown).
    let mut presence: Option<(InstanceKey, i64)> = None;
    let mut prev_key: Option<InstanceKey> = None;

    let user_id = rows.first().map(|r| r.user_id.clone()).unwrap_or_default();

    for row in rows {
        let ts = ts_to_i64(row.ts);
        let key = presence_key(row);
        if key != prev_key {
            // Close the outgoing segment, then start the new one.
            if let Some((ref k, start)) = presence {
                credit_segment(
                    k,
                    start,
                    ts,
                    range_start,
                    range_end,
                    session_intervals,
                    &user_id,
                    friend_seconds,
                );
            }
            prev_key = key.clone();
            presence = key.map(|k| (k, ts));
        }
    }
    // Close the final open segment at the range end.
    if let Some((ref k, start)) = presence {
        credit_segment(
            k,
            start,
            range_end,
            range_start,
            range_end,
            session_intervals,
            &user_id,
            friend_seconds,
        );
    }
}

/// The instance a friend-history row places its user in, if any.
fn presence_key(row: &crate::FriendHistoryEntry) -> Option<InstanceKey> {
    match row.event {
        FriendEvent::Offline | FriendEvent::Removed => return None,
        _ => {}
    }
    let world_id = if row.world_id.is_empty() {
        world_from_location(&row.location)
    } else {
        row.world_id.as_str()
    };
    if world_id.is_empty() {
        return None;
    }
    let instance_id = if row.instance_id.is_empty() {
        instance_from_location(&row.location)
    } else {
        row.instance_id.as_str()
    };
    Some(key_of(world_id, instance_id))
}

/// Add the overlap between one presence segment `[start, end)` (clipped to
/// the stats range) and same-key session intervals.
#[allow(clippy::too_many_arguments)]
fn credit_segment(
    key: &InstanceKey,
    start: i64,
    end: i64,
    range_start: i64,
    range_end: i64,
    session_intervals: &HashMap<InstanceKey, Vec<(i64, i64)>>,
    user_id: &str,
    friend_seconds: &mut HashMap<String, i64>,
) {
    let Some(intervals) = session_intervals.get(key) else {
        return;
    };
    let seg_start = start.max(range_start);
    let seg_end = end.min(range_end);
    if seg_end <= seg_start {
        return;
    }
    let mut seconds = 0i64;
    for &(s0, s1) in intervals {
        let overlap = seg_end.min(s1) - seg_start.max(s0);
        if overlap > 0 {
            seconds += overlap;
        }
    }
    if seconds > 0 {
        *friend_seconds.entry(user_id.to_string()).or_insert(0) += seconds;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::NewFriendHistoryEntry;
    use crate::sessions::NewSession;
    use crate::Store;
    use time::Duration;

    fn ts(secs: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::seconds(secs)
    }

    #[test]
    fn world_time_totals_and_top_worlds() {
        let store = Store::open_in_memory().unwrap();
        let sessions = store.sessions();
        let t0 = ts(1_700_000_000);
        sessions
            .open(&NewSession {
                started_at: t0,
                world_id: "wrld_a".into(),
                world_name: "World A".into(),
                instance_id: "1~region(us)".into(),
                location: "wrld_a:1~region(us)".into(),
            })
            .unwrap();
        let id_b = sessions
            .open(&NewSession {
                started_at: t0 + Duration::seconds(100),
                world_id: "wrld_b".into(),
                world_name: "World B".into(),
                ..NewSession::default()
            })
            .unwrap();
        sessions
            .open(&NewSession {
                started_at: t0 + Duration::seconds(200),
                world_id: "wrld_a".into(),
                ..NewSession::default()
            })
            .unwrap();
        // Close first two; third stays open (counts until now — far future,
        // so exclude it via the range `until` in the assertion below).
        sessions.close(1, t0 + Duration::seconds(100)).unwrap();
        sessions.close(id_b, t0 + Duration::seconds(160)).unwrap();

        let stats = store
            .stats()
            .compute(
                &StatsRange {
                    since: Some(t0),
                    until: Some(t0 + Duration::seconds(300)),
                },
                10,
            )
            .unwrap();
        // Session 1: 100s in wrld_a; session 2: 60s in wrld_b; session 3:
        // open, clipped to range end → 100s in wrld_a.
        assert_eq!(stats.total_seconds, 260);
        assert_eq!(stats.session_count, 3);
        assert_eq!(stats.top_worlds.len(), 2);
        assert_eq!(stats.top_worlds[0].world_id, "wrld_a");
        assert_eq!(stats.top_worlds[0].seconds, 200);
        assert_eq!(stats.top_worlds[0].world_name, "World A");
        assert_eq!(stats.top_worlds[0].sessions, 2);
        assert_eq!(stats.top_worlds[1].world_id, "wrld_b");
        assert_eq!(stats.top_worlds[1].seconds, 60);
    }

    #[test]
    fn friend_time_counts_instance_overlap_only() {
        let store = Store::open_in_memory().unwrap();
        let sessions = store.sessions();
        let history = store.history();
        let t0 = ts(1_700_000_000);

        // My session: wrld_a instance "1" for [t0, t0+100).
        let my = sessions
            .open(&NewSession {
                started_at: t0,
                world_id: "wrld_a".into(),
                instance_id: "1~region(us)".into(),
                location: "wrld_a:1~region(us)".into(),
                ..NewSession::default()
            })
            .unwrap();
        sessions.close(my, t0 + Duration::seconds(100)).unwrap();

        let friend_row =
            |event: FriendEvent, ts_off: i64, world: &str, inst: &str| NewFriendHistoryEntry {
                ts: t0 + Duration::seconds(ts_off),
                user_id: "usr_friend".into(),
                display_name: "Test Friend".into(),
                event,
                world_id: world.into(),
                instance_id: inst.into(),
                location: format!("{world}:{inst}"),
                ..NewFriendHistoryEntry::default()
            };
        // Friend in the SAME instance for the whole session → 100s.
        history
            .insert(&friend_row(
                FriendEvent::Location,
                0,
                "wrld_a",
                "1~region(us)",
            ))
            .unwrap();
        history
            .insert(&friend_row(FriendEvent::Offline, 200, "", ""))
            .unwrap();

        // A second friend in a DIFFERENT instance → 0s.
        let mut other = friend_row(FriendEvent::Location, 0, "wrld_a", "2~region(us)");
        other.user_id = "usr_other".into();
        history.insert(&other).unwrap();
        let mut other_off = friend_row(FriendEvent::Offline, 200, "", "");
        other_off.user_id = "usr_other".into();
        other_off.world_id.clear();
        other_off.instance_id.clear();
        other_off.location.clear();
        history.insert(&other_off).unwrap();

        let stats = store.stats().compute(&StatsRange::default(), 10).unwrap();
        assert_eq!(stats.top_friends.len(), 1);
        assert_eq!(stats.top_friends[0].user_id, "usr_friend");
        assert_eq!(stats.top_friends[0].display_name, "Test Friend");
        assert_eq!(stats.top_friends[0].seconds, 100);
    }

    #[test]
    fn friend_presence_outside_range_is_clipped() {
        let store = Store::open_in_memory().unwrap();
        let sessions = store.sessions();
        let history = store.history();
        let t0 = ts(1_700_000_000);

        let my = sessions
            .open(&NewSession {
                started_at: t0,
                world_id: "wrld_a".into(),
                instance_id: "1".into(),
                location: "wrld_a:1".into(),
                ..NewSession::default()
            })
            .unwrap();
        sessions.close(my, t0 + Duration::seconds(100)).unwrap();

        // Friend present [t0-50, t0+150): overlap with session inside
        // range [t0, t0+100) is still 100s — but with range [t0+25, t0+75)
        // it must be 50s.
        history
            .insert(&NewFriendHistoryEntry {
                ts: t0 - Duration::seconds(50),
                user_id: "usr_friend".into(),
                display_name: "Test Friend".into(),
                event: FriendEvent::Location,
                world_id: "wrld_a".into(),
                instance_id: "1".into(),
                location: "wrld_a:1".into(),
                ..NewFriendHistoryEntry::default()
            })
            .unwrap();
        history
            .insert(&NewFriendHistoryEntry {
                ts: t0 + Duration::seconds(150),
                user_id: "usr_friend".into(),
                display_name: "Test Friend".into(),
                event: FriendEvent::Offline,
                ..NewFriendHistoryEntry::default()
            })
            .unwrap();

        let stats = store
            .stats()
            .compute(
                &StatsRange {
                    since: Some(t0 + Duration::seconds(25)),
                    until: Some(t0 + Duration::seconds(75)),
                },
                10,
            )
            .unwrap();
        assert_eq!(stats.total_seconds, 50);
        assert_eq!(stats.top_friends[0].seconds, 50);
    }

    #[test]
    fn empty_store_yields_empty_stats() {
        let store = Store::open_in_memory().unwrap();
        let stats = store.stats().compute(&StatsRange::default(), 10).unwrap();
        assert_eq!(stats.total_seconds, 0);
        assert_eq!(stats.session_count, 0);
        assert!(stats.top_worlds.is_empty());
        assert!(stats.top_friends.is_empty());
    }
}
