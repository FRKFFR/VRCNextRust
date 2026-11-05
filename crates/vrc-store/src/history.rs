//! Friend history repository: presence and location events for friends,
//! persisted from `vrc-pipeline` events.

use rusqlite::{params, params_from_iter, Connection, Row};
use time::OffsetDateTime;

use crate::{i64_to_ts, lock, ts_to_i64, Conn, StoreError, DEFAULT_PAGE_SIZE};

/// Friend-history event kinds (derived from `vrc_pipeline::PipelineEvent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FriendEvent {
    /// `friend-online`: the friend came online.
    Online,
    /// `friend-active`: the friend is active on web/app.
    Active,
    /// `friend-offline`: the friend went offline.
    Offline,
    /// `friend-location`: the friend's instance changed.
    Location,
    /// `friend-add`: a new friend was added.
    Added,
    /// `friend-delete`: a friend was removed.
    Removed,
    /// `friend-update`: the friend's profile/status changed.
    Updated,
}

impl FriendEvent {
    fn as_str(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Active => "active",
            Self::Offline => "offline",
            Self::Location => "location",
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Updated => "updated",
        }
    }

    fn from_str(raw: &str) -> Option<Self> {
        Some(match raw {
            "online" => Self::Online,
            "active" => Self::Active,
            "offline" => Self::Offline,
            "location" => Self::Location,
            "added" => Self::Added,
            "removed" => Self::Removed,
            "updated" => Self::Updated,
            _ => return None,
        })
    }
}

/// One friend-history row.
#[derive(Debug, Clone, PartialEq)]
pub struct FriendHistoryEntry {
    /// Row id (monotonic; newest rows have the highest id).
    pub id: i64,
    /// When the event happened.
    pub ts: OffsetDateTime,
    /// `usr_…` friend id.
    pub user_id: String,
    /// Friend display name at event time; empty when unknown.
    pub display_name: String,
    /// Event kind.
    pub event: FriendEvent,
    /// `wrld_…` world id when the friend was in a world; empty otherwise.
    pub world_id: String,
    /// Instance spec after `wrld_…:` (may contain `~` parts); empty when
    /// the friend was not in a named instance.
    pub instance_id: String,
    /// Full `wrld_…:instance` location; empty when not in a world.
    pub location: String,
    /// `last_platform` for offline events; empty otherwise.
    pub platform: String,
}

/// Fields for a new friend-history row.
#[derive(Debug, Clone)]
pub struct NewFriendHistoryEntry {
    /// When the event happened.
    pub ts: OffsetDateTime,
    /// `usr_…` friend id.
    pub user_id: String,
    /// Friend display name at event time.
    pub display_name: String,
    /// Event kind.
    pub event: FriendEvent,
    /// `wrld_…` world id when in a world.
    pub world_id: String,
    /// Instance spec.
    pub instance_id: String,
    /// Full `wrld_…:instance` location.
    pub location: String,
    /// `last_platform` for offline events.
    pub platform: String,
}

impl Default for NewFriendHistoryEntry {
    fn default() -> Self {
        Self {
            ts: OffsetDateTime::UNIX_EPOCH,
            user_id: String::new(),
            display_name: String::new(),
            event: FriendEvent::Online,
            world_id: String::new(),
            instance_id: String::new(),
            location: String::new(),
            platform: String::new(),
        }
    }
}

/// Filters + paging for [`HistoryRepo::list`]/[`HistoryRepo::count`].
#[derive(Debug, Clone, Default)]
pub struct FriendHistoryFilter {
    /// Restrict to one friend (`usr_…`).
    pub user_id: Option<String>,
    /// Case-insensitive substring matched against user id, display name,
    /// world id, instance and location.
    pub query: Option<String>,
    /// Rows to skip (0 = first page).
    pub offset: u32,
    /// Page size; `0` uses [`DEFAULT_PAGE_SIZE`].
    pub limit: u32,
}

/// Repository for the `friend_history` table.
#[derive(Clone)]
pub struct HistoryRepo {
    pub(crate) conn: Conn,
}

impl HistoryRepo {
    pub(crate) fn new(conn: Conn) -> Self {
        Self { conn }
    }

    /// Insert one row; returns its id.
    pub fn insert(&self, entry: &NewFriendHistoryEntry) -> Result<i64, StoreError> {
        let conn = lock(&self.conn);
        conn.execute(
            "INSERT INTO friend_history
                (ts, user_id, display_name, event, world_id, instance_id,
                 location, platform)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                ts_to_i64(entry.ts),
                entry.user_id,
                entry.display_name,
                entry.event.as_str(),
                entry.world_id,
                entry.instance_id,
                entry.location,
                entry.platform,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Newest-first page of rows matching `filter`.
    pub fn list(
        &self,
        filter: &FriendHistoryFilter,
    ) -> Result<Vec<FriendHistoryEntry>, StoreError> {
        let conn = lock(&self.conn);
        let (where_sql, binds) = filter_clauses(filter);
        let sql = format!(
            "SELECT id, ts, user_id, display_name, event, world_id,
                    instance_id, location, platform
             FROM friend_history {where_sql}
             ORDER BY ts DESC, id DESC
             LIMIT ? OFFSET ?"
        );
        let limit = i64::from(if filter.limit == 0 {
            DEFAULT_PAGE_SIZE
        } else {
            filter.limit
        });
        let offset = i64::from(filter.offset);
        let mut values: Vec<rusqlite::types::Value> =
            binds.into_iter().map(Bind::into_value).collect();
        values.push(rusqlite::types::Value::Integer(limit));
        values.push(rusqlite::types::Value::Integer(offset));
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values), map_entry)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::Sql)
    }

    /// Total rows matching `filter` (ignores `offset`/`limit`).
    pub fn count(&self, filter: &FriendHistoryFilter) -> Result<u64, StoreError> {
        let conn = lock(&self.conn);
        let (where_sql, binds) = filter_clauses(filter);
        let sql = format!("SELECT COUNT(*) FROM friend_history {where_sql}");
        let values: Vec<rusqlite::types::Value> = binds.into_iter().map(Bind::into_value).collect();
        let count: i64 = conn
            .prepare(&sql)?
            .query_row(params_from_iter(values), |r| r.get(0))?;
        Ok(count.max(0) as u64)
    }
}

fn filter_clauses(filter: &FriendHistoryFilter) -> (String, Vec<Bind>) {
    let mut sql = String::new();
    let mut binds = Vec::new();
    if let Some(user_id) = filter.user_id.as_deref().filter(|u| !u.is_empty()) {
        sql.push_str(" WHERE user_id = ?");
        binds.push(Bind::UserId(user_id.to_string()));
    }
    if filter
        .query
        .as_deref()
        .is_some_and(|q| !q.trim().is_empty())
    {
        if sql.is_empty() {
            sql.push_str(" WHERE");
        } else {
            sql.push_str(" AND");
        }
        sql.push_str(
            " instr(lower(user_id || char(31) || display_name || char(31) || world_id || char(31) || instance_id || char(31) || location), lower(?)) > 0",
        );
        binds.push(Bind::Query(filter.query.clone().unwrap()));
    }
    (sql, binds)
}

enum Bind {
    UserId(String),
    Query(String),
}

impl Bind {
    fn into_value(self) -> rusqlite::types::Value {
        match self {
            Self::UserId(v) | Self::Query(v) => rusqlite::types::Value::Text(v),
        }
    }
}

fn map_entry(row: &Row<'_>) -> rusqlite::Result<FriendHistoryEntry> {
    let event_raw: String = row.get(4)?;
    Ok(FriendHistoryEntry {
        id: row.get(0)?,
        ts: i64_to_ts(row.get::<_, i64>(1)?),
        user_id: row.get(2)?,
        display_name: row.get(3)?,
        event: FriendEvent::from_str(&event_raw).unwrap_or(FriendEvent::Updated),
        world_id: row.get(5)?,
        instance_id: row.get(6)?,
        location: row.get(7)?,
        platform: row.get(8)?,
    })
}

/// Every friend-history row with `ts <= until` (unix seconds), grouped by
/// user and ordered by time — the input the stats repository needs to
/// reconstruct per-friend presence intervals.
pub(crate) fn rows_up_to(
    conn: &Connection,
    until: i64,
) -> Result<Vec<FriendHistoryEntry>, StoreError> {
    let rows = conn
        .prepare(
            "SELECT id, ts, user_id, display_name, event, world_id,
                    instance_id, location, platform
             FROM friend_history
             WHERE ts <= ?1
             ORDER BY user_id, ts, id",
        )?
        .query_map(params![until], map_entry)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use time::Duration;

    fn entry(ts: OffsetDateTime, user: &str, event: FriendEvent) -> NewFriendHistoryEntry {
        NewFriendHistoryEntry {
            ts,
            user_id: user.to_string(),
            display_name: format!("Friend {user}"),
            event,
            world_id: "wrld_00000000-0000-0000-0000-000000000001".to_string(),
            instance_id: "1234~region(us)".to_string(),
            location: "wrld_00000000-0000-0000-0000-000000000001:1234~region(us)".to_string(),
            ..NewFriendHistoryEntry::default()
        }
    }

    #[test]
    fn insert_list_filters_by_user_and_pages() {
        let store = Store::open_in_memory().unwrap();
        let repo = store.history();
        let t0 = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_700_000_000);
        for i in 0..3i64 {
            repo.insert(&entry(
                t0 + Duration::seconds(i),
                "usr_a",
                FriendEvent::Online,
            ))
            .unwrap();
            repo.insert(&entry(
                t0 + Duration::seconds(i),
                "usr_b",
                FriendEvent::Location,
            ))
            .unwrap();
        }

        let a_only = repo
            .list(&FriendHistoryFilter {
                user_id: Some("usr_a".to_string()),
                ..FriendHistoryFilter::default()
            })
            .unwrap();
        assert_eq!(a_only.len(), 3);
        assert!(a_only.iter().all(|e| e.user_id == "usr_a"));
        assert!(a_only.iter().all(|e| e.event == FriendEvent::Online));

        let page = repo
            .list(&FriendHistoryFilter {
                offset: 4,
                limit: 4,
                ..FriendHistoryFilter::default()
            })
            .unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(repo.count(&FriendHistoryFilter::default()).unwrap(), 6);
        assert_eq!(
            repo.count(&FriendHistoryFilter {
                user_id: Some("usr_b".to_string()),
                ..FriendHistoryFilter::default()
            })
            .unwrap(),
            3
        );
    }

    #[test]
    fn list_orders_by_timestamp_not_insertion_order() {
        let store = Store::open_in_memory().unwrap();
        let repo = store.history();
        let t0 = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_700_000_000);
        // Insert out of order so row ids disagree with timestamp order.
        let mut ids = Vec::new();
        for secs in [30, 10, 20] {
            ids.push(
                repo.insert(&entry(
                    t0 + Duration::seconds(secs),
                    "usr_a",
                    FriendEvent::Online,
                ))
                .unwrap(),
            );
        }
        let list = repo.list(&FriendHistoryFilter::default()).unwrap();
        // ts DESC, id DESC: 30s (ids[0]), 20s (ids[2]), 10s (ids[1]).
        let order: Vec<i64> = list.iter().map(|e| e.id).collect();
        assert_eq!(order, vec![ids[0], ids[2], ids[1]]);
    }
}
