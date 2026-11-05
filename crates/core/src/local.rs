//! Serialized access to the local SQLite store (M3).
//!
//! [`vrc_store::Store`] is synchronous; every write and query is funnelled
//! through one dedicated OS thread ([`spawn_store_thread`]) so session
//! open/close ordering is preserved and the Tokio runtime is never blocked.
//! The thread owns the "currently open world session" so `JoinedWorld` /
//! `LeftWorld` pairs from the log tailer can never interleave incorrectly.

use std::collections::HashMap;
use std::sync::mpsc;

use time::OffsetDateTime;
use tokio::sync::oneshot;
use vrc_logs::LogEvent;
use vrc_pipeline::PipelineEvent;
use vrc_store::{
    FriendEvent, FriendHistoryFilter, NewFriendHistoryEntry, NewSession, NewTimelineEntry, Stats,
    StatsRange, Store, TimelineFilter, TimelineKind,
};

use crate::state::{Friend, HistoryPage, TimelinePage};

/// Result handed back to the async side of a store query.
pub type QueryResult<T> = Result<T, String>;

/// One unit of work for the store thread.
pub enum StoreOp {
    /// Insert a personal-timeline row (a parsed log event).
    Timeline(NewTimelineEntry),
    /// Insert a friend-history row (a pipeline event).
    History(NewFriendHistoryEntry),
    /// The local player joined a world: close the open session (if any) at
    /// `ts` and open a new one. A name-only event (`Entering Room:`, which
    /// follows `Joining wrld_…` for the same visit) instead just names the
    /// open session — or opens a name-only session when none is open.
    WorldJoin {
        /// Event timestamp.
        ts: OffsetDateTime,
        /// `wrld_…` id (empty for name-only joins).
        world_id: String,
        /// World display name (empty when unknown).
        world_name: String,
        /// Instance spec after `wrld_…:`.
        instance_id: String,
        /// Full `wrld_…:instance` location.
        location: String,
    },
    /// The local player left the world: close the open session at `ts`.
    WorldLeft {
        /// Event timestamp.
        ts: OffsetDateTime,
    },
    /// Close every open session at `ts` — startup cleanup for sessions
    /// orphaned by a previous run, and tailer restarts.
    CloseAllSessions {
        /// Close timestamp.
        ts: OffsetDateTime,
    },
    /// Paged timeline query.
    QueryTimeline {
        /// Filters + paging.
        filter: TimelineFilter,
        /// Where the result goes.
        reply: oneshot::Sender<QueryResult<TimelinePage>>,
    },
    /// Paged friend-history query.
    QueryHistory {
        /// Filters + paging.
        filter: FriendHistoryFilter,
        /// Where the result goes.
        reply: oneshot::Sender<QueryResult<HistoryPage>>,
    },
    /// Time-spent statistics.
    QueryStats {
        /// Time bounds.
        range: StatsRange,
        /// Where the result goes.
        reply: oneshot::Sender<QueryResult<Stats>>,
    },
}

/// Spawn the single store thread; returns its command sender. The thread
/// exits once every sender is dropped (app shutdown).
pub(crate) fn spawn_store_thread(store: Store) -> mpsc::Sender<StoreOp> {
    let (tx, rx) = mpsc::channel::<StoreOp>();
    std::thread::Builder::new()
        .name("vrc-store".to_string())
        .spawn(move || store_loop(store, rx))
        .expect("spawn vrc-store thread");
    tx
}

fn store_loop(store: Store, rx: mpsc::Receiver<StoreOp>) {
    // The session row currently open, tracked here so `WorldJoin` /
    // `WorldLeft` ops stay correctly ordered even though they were queued
    // from different async tasks.
    let mut open_session: Option<i64> = None;
    while let Ok(op) = rx.recv() {
        match op {
            StoreOp::Timeline(entry) => {
                if let Err(e) = store.timeline().insert(&entry) {
                    tracing::debug!("vrc-core: timeline insert failed: {e}");
                }
            }
            StoreOp::History(entry) => {
                if let Err(e) = store.history().insert(&entry) {
                    tracing::debug!("vrc-core: history insert failed: {e}");
                }
            }
            StoreOp::WorldJoin {
                ts,
                world_id,
                world_name,
                instance_id,
                location,
            } => {
                // `Entering Room: <name>` carries no world id and refines
                // the session opened by the preceding `Joining wrld_…`.
                if world_id.is_empty() && !world_name.is_empty() && open_session.is_some() {
                    if let Some(id) = open_session {
                        if let Err(e) = store.sessions().update_world(id, "", &world_name, "", "") {
                            tracing::debug!("vrc-core: session name update failed: {e}");
                        }
                    }
                    continue;
                }
                // A fresh world join retires the previous session at this
                // instant (covers the "new JoinedWorld" close rule).
                if let Some(id) = open_session.take() {
                    if let Err(e) = store.sessions().close(id, ts) {
                        tracing::debug!("vrc-core: session close failed: {e}");
                    }
                }
                open_session = store
                    .sessions()
                    .open(&NewSession {
                        started_at: ts,
                        world_id,
                        world_name,
                        instance_id,
                        location,
                    })
                    .map_err(|e| {
                        tracing::debug!("vrc-core: session open failed: {e}");
                        e
                    })
                    .ok();
            }
            StoreOp::WorldLeft { ts } => {
                if let Some(id) = open_session.take() {
                    if let Err(e) = store.sessions().close(id, ts) {
                        tracing::debug!("vrc-core: session close failed: {e}");
                    }
                }
            }
            StoreOp::CloseAllSessions { ts } => {
                open_session = None;
                if let Err(e) = store.sessions().close_all_open(ts) {
                    tracing::debug!("vrc-core: session cleanup failed: {e}");
                }
            }
            StoreOp::QueryTimeline { filter, reply } => {
                let _ = reply.send(query_timeline(&store, &filter));
            }
            StoreOp::QueryHistory { filter, reply } => {
                let _ = reply.send(query_history(&store, &filter));
            }
            StoreOp::QueryStats { range, reply } => {
                let _ = reply.send(
                    store
                        .stats()
                        .compute(&range, STATS_TOP_N)
                        .map_err(|e| e.to_string()),
                );
            }
        }
    }
}

/// Top-N cap for the friends UI / CLI stats views.
const STATS_TOP_N: usize = 10;

fn query_timeline(store: &Store, filter: &TimelineFilter) -> QueryResult<TimelinePage> {
    let repo = store.timeline();
    let entries = repo.list(filter).map_err(|e| e.to_string())?;
    let total = repo.count(filter).map_err(|e| e.to_string())?;
    Ok(TimelinePage { entries, total })
}

fn query_history(store: &Store, filter: &FriendHistoryFilter) -> QueryResult<HistoryPage> {
    let repo = store.history();
    let entries = repo.list(filter).map_err(|e| e.to_string())?;
    let total = repo.count(filter).map_err(|e| e.to_string())?;
    Ok(HistoryPage { entries, total })
}

/// Convert a parsed log event into the store ops it implies: always a
/// timeline row, plus session bookkeeping for world joins/leaves.
pub(crate) fn ops_for_log_event(event: &LogEvent) -> Vec<StoreOp> {
    let at = event.at();
    let mut ops = Vec::with_capacity(2);
    match event {
        LogEvent::JoinedWorld {
            world_id,
            world_name,
            instance_id,
            location,
            ..
        } => {
            ops.push(StoreOp::Timeline(NewTimelineEntry {
                ts: at,
                kind: TimelineKind::JoinedWorld,
                world_id: world_id.clone(),
                world_name: world_name.clone(),
                instance_id: instance_id.clone(),
                location: location.clone(),
                ..NewTimelineEntry::default()
            }));
            ops.push(StoreOp::WorldJoin {
                ts: at,
                world_id: world_id.clone(),
                world_name: world_name.clone(),
                instance_id: instance_id.clone(),
                location: location.clone(),
            });
        }
        LogEvent::LeftWorld { .. } => {
            ops.push(StoreOp::Timeline(NewTimelineEntry {
                ts: at,
                kind: TimelineKind::LeftWorld,
                ..NewTimelineEntry::default()
            }));
            ops.push(StoreOp::WorldLeft { ts: at });
        }
        LogEvent::PlayerJoined {
            user_id,
            display_name,
            ..
        } => ops.push(StoreOp::Timeline(NewTimelineEntry {
            ts: at,
            kind: TimelineKind::PlayerJoined,
            user_id: user_id.clone(),
            display_name: display_name.clone(),
            ..NewTimelineEntry::default()
        })),
        LogEvent::PlayerLeft {
            user_id,
            display_name,
            ..
        } => ops.push(StoreOp::Timeline(NewTimelineEntry {
            ts: at,
            kind: TimelineKind::PlayerLeft,
            user_id: user_id.clone(),
            display_name: display_name.clone(),
            ..NewTimelineEntry::default()
        })),
        LogEvent::PhotoTaken { path, .. } => ops.push(StoreOp::Timeline(NewTimelineEntry {
            ts: at,
            kind: TimelineKind::PhotoTaken,
            detail: path.clone(),
            ..NewTimelineEntry::default()
        })),
    }
    ops
}

/// Map a friend-related pipeline event to a friend-history row. Returns
/// `None` for events that carry no friend presence (notifications, user
/// updates, connection status, unknown frames).
///
/// `friends` is the pre-event friends map, used for display-name/platform
/// fallbacks when the event payload is degraded.
pub(crate) fn history_entry_for(
    event: &PipelineEvent,
    friends: &HashMap<String, Friend>,
) -> Option<NewFriendHistoryEntry> {
    let mut entry = NewFriendHistoryEntry {
        ts: OffsetDateTime::now_utc(),
        ..NewFriendHistoryEntry::default()
    };
    match event {
        PipelineEvent::FriendOnline { user_id, user } => {
            entry.event = FriendEvent::Online;
            entry.user_id = pick_id(user_id, &user.id);
            entry.display_name = user.display_name.clone();
            fill_location(&mut entry, user.location.as_str(), user.world_id.as_str());
            entry.platform = user.last_platform.clone();
        }
        PipelineEvent::FriendActive { user_id, user } => {
            entry.event = FriendEvent::Active;
            entry.user_id = pick_id(user_id, &user.id);
            entry.display_name = user.display_name.clone();
            fill_location(&mut entry, user.location.as_str(), user.world_id.as_str());
            entry.platform = user.last_platform.clone();
        }
        PipelineEvent::FriendOffline { user_id, platform } => {
            entry.event = FriendEvent::Offline;
            entry.user_id = user_id.clone();
            entry.platform = platform.clone();
        }
        PipelineEvent::FriendLocation {
            user_id,
            location,
            world_id,
            user,
            ..
        } => {
            entry.event = FriendEvent::Location;
            entry.user_id = pick_id(
                user_id,
                &user.as_ref().map(|u| u.id.clone()).unwrap_or_default(),
            );
            if let Some(user) = user.as_ref() {
                entry.display_name = user.display_name.clone();
                entry.platform = user.last_platform.clone();
            }
            fill_location(&mut entry, location, world_id);
        }
        PipelineEvent::FriendUpdate { user_id, user } => {
            entry.event = FriendEvent::Updated;
            entry.user_id = pick_id(user_id, &user.id);
            entry.display_name = user.display_name.clone();
            fill_location(&mut entry, user.location.as_str(), user.world_id.as_str());
            entry.platform = user.last_platform.clone();
        }
        PipelineEvent::FriendAdd { user_id, user } => {
            entry.event = FriendEvent::Added;
            entry.user_id = pick_id(user_id, &user.id);
            entry.display_name = user.display_name.clone();
            fill_location(&mut entry, user.location.as_str(), user.world_id.as_str());
            entry.platform = user.last_platform.clone();
        }
        PipelineEvent::FriendDelete { user_id } => {
            entry.event = FriendEvent::Removed;
            entry.user_id = user_id.clone();
        }
        _ => return None,
    }

    if entry.user_id.is_empty() {
        return None;
    }
    // Fill name/platform gaps from the friends map (degraded payloads).
    if let Some(friend) = friends.get(&entry.user_id) {
        if entry.display_name.is_empty() {
            entry.display_name = friend.display_name.clone();
        }
        if entry.platform.is_empty() {
            entry.platform = friend.last_platform.clone();
        }
    }
    Some(entry)
}

/// `user_id` wins; fall back to the payload's own id (same rule the state
/// merge uses).
fn pick_id(event_id: &str, user_id: &str) -> String {
    if !event_id.is_empty() {
        event_id.to_string()
    } else {
        user_id.to_string()
    }
}

/// Fill `world_id`/`instance_id`/`location` on a history entry from a
/// `wrld_…[:instance]` location tag and an optional explicit world id.
fn fill_location(entry: &mut NewFriendHistoryEntry, location: &str, world_id: &str) {
    entry.location = location.to_string();
    let (parsed_world, parsed_instance) = split_location(location);
    entry.world_id = if !world_id.is_empty() {
        world_id.to_string()
    } else {
        parsed_world
    };
    entry.instance_id = parsed_instance;
}

/// `wrld_…:instance~…` → `(wrld_…, instance~…)`; anything else → empty.
fn split_location(location: &str) -> (String, String) {
    if let Some((head, instance)) = location.split_once(':') {
        if head.starts_with("wrld_") {
            return (head.to_string(), instance.to_string());
        }
    } else if location.starts_with("wrld_") {
        return (location.to_string(), String::new());
    }
    (String::new(), String::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
    use vrc_api::User;

    fn at() -> OffsetDateTime {
        PrimitiveDateTime::new(
            Date::from_calendar_date(2024, Month::May, 14).unwrap(),
            Time::from_hms(20, 45, 1).unwrap(),
        )
        .assume_offset(UtcOffset::UTC)
    }

    #[test]
    fn log_events_map_to_expected_ops() {
        let joined = LogEvent::JoinedWorld {
            world_id: "wrld_a".into(),
            world_name: String::new(),
            instance_id: "1~region(us)".into(),
            location: "wrld_a:1~region(us)".into(),
            at: at(),
        };
        let ops = ops_for_log_event(&joined);
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], StoreOp::Timeline(_)));
        assert!(matches!(ops[1], StoreOp::WorldJoin { .. }));

        let photo = LogEvent::PhotoTaken {
            path: "C:\\pic.png".into(),
            at: at(),
        };
        let ops = ops_for_log_event(&photo);
        assert_eq!(ops.len(), 1);
        let StoreOp::Timeline(entry) = &ops[0] else {
            panic!("expected Timeline op");
        };
        assert_eq!(entry.kind, TimelineKind::PhotoTaken);
        assert_eq!(entry.detail, "C:\\pic.png");

        let left = LogEvent::LeftWorld { at: at() };
        let ops = ops_for_log_event(&left);
        assert!(ops.iter().any(|op| matches!(op, StoreOp::WorldLeft { .. })));
    }

    #[test]
    fn pipeline_friend_events_become_history_rows() {
        let friends: HashMap<String, Friend> = HashMap::new();
        let user = User {
            id: "usr_friend".into(),
            display_name: "Test Friend".into(),
            location: "wrld_a:55~region(us)".into(),
            world_id: "wrld_a".into(),
            last_platform: "standalonewindows".into(),
            ..User::default()
        };
        let entry = history_entry_for(
            &PipelineEvent::FriendLocation {
                user_id: "usr_friend".into(),
                location: "wrld_a:55~region(us)".into(),
                traveling_to_location: String::new(),
                world_id: "wrld_a".into(),
                user: Some(user),
            },
            &friends,
        )
        .expect("location event should produce a row");
        assert_eq!(entry.event, FriendEvent::Location);
        assert_eq!(entry.user_id, "usr_friend");
        assert_eq!(entry.display_name, "Test Friend");
        assert_eq!(entry.world_id, "wrld_a");
        assert_eq!(entry.instance_id, "55~region(us)");
        assert_eq!(entry.location, "wrld_a:55~region(us)");

        // Offline carries the platform and no location.
        let entry = history_entry_for(
            &PipelineEvent::FriendOffline {
                user_id: "usr_friend".into(),
                platform: "android".into(),
            },
            &friends,
        )
        .unwrap();
        assert_eq!(entry.event, FriendEvent::Offline);
        assert_eq!(entry.platform, "android");
        assert!(entry.world_id.is_empty());

        // Non-presence events produce no row.
        assert!(history_entry_for(&PipelineEvent::Connected, &friends).is_none());
        assert!(history_entry_for(
            &PipelineEvent::FriendDelete {
                user_id: String::new()
            },
            &friends
        )
        .is_none());
    }

    #[test]
    fn degraded_payload_falls_back_to_friends_map() {
        let mut friends = HashMap::new();
        friends.insert(
            "usr_known".to_string(),
            Friend {
                display_name: "Known Name".into(),
                last_platform: "ios".into(),
                ..Friend::default()
            },
        );
        // friend-online with an empty user payload still records the id
        // and borrows name/platform from the friends map.
        let entry = history_entry_for(
            &PipelineEvent::FriendOnline {
                user_id: "usr_known".into(),
                user: User::default(),
            },
            &friends,
        )
        .unwrap();
        assert_eq!(entry.display_name, "Known Name");
        assert_eq!(entry.platform, "ios");
    }
}
