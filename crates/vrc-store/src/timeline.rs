//! Personal timeline repository: world joins/leaves, player joins/leaves
//! and photos parsed from VRChat output logs.

use rusqlite::{params, params_from_iter, Row};
use time::OffsetDateTime;

use crate::{i64_to_ts, lock, ts_to_i64, Conn, StoreError, DEFAULT_PAGE_SIZE};

/// Kinds of personal-timeline rows (mirrors `vrc_logs::LogEvent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineKind {
    /// The local player joined a world.
    JoinedWorld,
    /// The local player left the world.
    LeftWorld,
    /// Another player joined the local player's instance.
    PlayerJoined,
    /// Another player left the instance.
    PlayerLeft,
    /// A screenshot/photo file was written.
    PhotoTaken,
}

impl TimelineKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::JoinedWorld => "joined_world",
            Self::LeftWorld => "left_world",
            Self::PlayerJoined => "player_joined",
            Self::PlayerLeft => "player_left",
            Self::PhotoTaken => "photo_taken",
        }
    }

    fn from_str(raw: &str) -> Option<Self> {
        Some(match raw {
            "joined_world" => Self::JoinedWorld,
            "left_world" => Self::LeftWorld,
            "player_joined" => Self::PlayerJoined,
            "player_left" => Self::PlayerLeft,
            "photo_taken" => Self::PhotoTaken,
            _ => return None,
        })
    }
}

/// One personal-timeline row.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineEntry {
    /// Row id (monotonic; newest rows have the highest id).
    pub id: i64,
    /// When the event happened (log timestamp).
    pub ts: OffsetDateTime,
    /// Event kind.
    pub kind: TimelineKind,
    /// `usr_…` id for player events; empty otherwise.
    pub user_id: String,
    /// Display name for player events; empty otherwise.
    pub display_name: String,
    /// `wrld_…` world id for world events; empty otherwise.
    pub world_id: String,
    /// World display name when known; empty otherwise.
    pub world_name: String,
    /// Instance spec (`1234~hidden(…)~…`); empty for bare world joins.
    pub instance_id: String,
    /// Full `wrld_…:instance` location; empty when unknown.
    pub location: String,
    /// Extra detail — the file path for `PhotoTaken`; empty otherwise.
    pub detail: String,
}

/// Fields for a new timeline row (id is assigned by the database).
#[derive(Debug, Clone)]
pub struct NewTimelineEntry {
    /// When the event happened.
    pub ts: OffsetDateTime,
    /// Event kind.
    pub kind: TimelineKind,
    /// `usr_…` id for player events.
    pub user_id: String,
    /// Display name for player events.
    pub display_name: String,
    /// `wrld_…` world id for world events.
    pub world_id: String,
    /// World display name when known.
    pub world_name: String,
    /// Instance spec.
    pub instance_id: String,
    /// Full `wrld_…:instance` location.
    pub location: String,
    /// Extra detail (photo path).
    pub detail: String,
}

impl Default for NewTimelineEntry {
    fn default() -> Self {
        Self {
            ts: OffsetDateTime::UNIX_EPOCH,
            kind: TimelineKind::JoinedWorld,
            user_id: String::new(),
            display_name: String::new(),
            world_id: String::new(),
            world_name: String::new(),
            instance_id: String::new(),
            location: String::new(),
            detail: String::new(),
        }
    }
}

/// Filters + paging for [`TimelineRepo::list`]/[`TimelineRepo::count`].
#[derive(Debug, Clone, Default)]
pub struct TimelineFilter {
    /// Restrict to one event kind.
    pub kind: Option<TimelineKind>,
    /// Case-insensitive substring matched against user id, display name,
    /// world id/name, instance, location and detail.
    pub query: Option<String>,
    /// Rows to skip (0 = first page).
    pub offset: u32,
    /// Page size; `0` uses [`DEFAULT_PAGE_SIZE`].
    pub limit: u32,
}

/// Repository for the `timeline` table.
#[derive(Clone)]
pub struct TimelineRepo {
    pub(crate) conn: Conn,
}

impl TimelineRepo {
    pub(crate) fn new(conn: Conn) -> Self {
        Self { conn }
    }

    /// Insert one row; returns its id.
    pub fn insert(&self, entry: &NewTimelineEntry) -> Result<i64, StoreError> {
        let conn = lock(&self.conn);
        conn.execute(
            "INSERT INTO timeline
                (ts, kind, user_id, display_name, world_id, world_name,
                 instance_id, location, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                ts_to_i64(entry.ts),
                entry.kind.as_str(),
                entry.user_id,
                entry.display_name,
                entry.world_id,
                entry.world_name,
                entry.instance_id,
                entry.location,
                entry.detail,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Newest-first page of rows matching `filter`.
    pub fn list(&self, filter: &TimelineFilter) -> Result<Vec<TimelineEntry>, StoreError> {
        let conn = lock(&self.conn);
        let (where_sql, binds) = filter_clauses(filter);
        let sql = format!(
            "SELECT id, ts, kind, user_id, display_name, world_id, world_name,
                    instance_id, location, detail
             FROM timeline {where_sql}
             ORDER BY id DESC
             LIMIT ? OFFSET ?"
        );
        let mut stmt = conn.prepare(&sql)?;
        let limit = effective_limit(filter.limit);
        let offset = i64::from(filter.offset);
        let rows = stmt.query_map(
            params_from_iter(params_from_binds(binds, limit, offset)),
            map_entry,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::Sql)
    }

    /// Total rows matching `filter` (ignores `offset`/`limit`).
    pub fn count(&self, filter: &TimelineFilter) -> Result<u64, StoreError> {
        let conn = lock(&self.conn);
        let (where_sql, binds) = filter_clauses(filter);
        let sql = format!("SELECT COUNT(*) FROM timeline {where_sql}");
        let mut stmt = conn.prepare(&sql)?;
        let count: i64 = stmt.query_row(params_from_iter(params_from_binds_count(binds)), |r| {
            r.get(0)
        })?;
        Ok(count.max(0) as u64)
    }
}

fn effective_limit(limit: u32) -> i64 {
    i64::from(if limit == 0 { DEFAULT_PAGE_SIZE } else { limit })
}

/// WHERE fragment + positional binds for the optional filters. `kind` is
/// bind 1, `query` bind 2; LIMIT/OFFSET are appended by the caller.
fn filter_clauses(filter: &TimelineFilter) -> (String, Vec<Bind>) {
    let mut sql = String::new();
    let mut binds = Vec::new();
    if let Some(kind) = filter.kind {
        sql.push_str(" WHERE kind = ?");
        binds.push(Bind::Kind(kind));
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
        // Case-insensitive substring over every display column; char(31)
        // separates fields so a query cannot match across boundaries.
        sql.push_str(
            " instr(lower(user_id || char(31) || display_name || char(31) || world_id || char(31) || world_name || char(31) || instance_id || char(31) || location || char(31) || detail), lower(?)) > 0",
        );
        binds.push(Bind::Query(filter.query.clone().unwrap()));
    }
    (sql, binds)
}

enum Bind {
    Kind(TimelineKind),
    Query(String),
}

fn params_from_binds(binds: Vec<Bind>, limit: i64, offset: i64) -> Vec<rusqlite::types::Value> {
    let mut values: Vec<rusqlite::types::Value> = binds
        .into_iter()
        .map(|b| match b {
            Bind::Kind(k) => rusqlite::types::Value::Text(k.as_str().to_string()),
            Bind::Query(q) => rusqlite::types::Value::Text(q),
        })
        .collect();
    values.push(rusqlite::types::Value::Integer(limit));
    values.push(rusqlite::types::Value::Integer(offset));
    values
}

fn params_from_binds_count(binds: Vec<Bind>) -> Vec<rusqlite::types::Value> {
    binds
        .into_iter()
        .map(|b| match b {
            Bind::Kind(k) => rusqlite::types::Value::Text(k.as_str().to_string()),
            Bind::Query(q) => rusqlite::types::Value::Text(q),
        })
        .collect()
}

fn map_entry(row: &Row<'_>) -> rusqlite::Result<TimelineEntry> {
    let kind_raw: String = row.get(2)?;
    Ok(TimelineEntry {
        id: row.get(0)?,
        ts: i64_to_ts(row.get::<_, i64>(1)?),
        kind: TimelineKind::from_str(&kind_raw).unwrap_or(TimelineKind::JoinedWorld),
        user_id: row.get(3)?,
        display_name: row.get(4)?,
        world_id: row.get(5)?,
        world_name: row.get(6)?,
        instance_id: row.get(7)?,
        location: row.get(8)?,
        detail: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use time::Duration;

    fn entry(kind: TimelineKind, ts: OffsetDateTime, name: &str) -> NewTimelineEntry {
        NewTimelineEntry {
            ts,
            kind,
            display_name: name.to_string(),
            ..NewTimelineEntry::default()
        }
    }

    #[test]
    fn insert_list_pages_and_filters() {
        let store = Store::open_in_memory().unwrap();
        let repo = store.timeline();
        let t0 = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_700_000_000);
        for i in 0..5i64 {
            repo.insert(&entry(
                TimelineKind::PlayerJoined,
                t0 + Duration::seconds(i),
                &format!("User {i}"),
            ))
            .unwrap();
        }
        repo.insert(&entry(TimelineKind::PhotoTaken, t0, ""))
            .unwrap();

        let all = repo.list(&TimelineFilter::default()).unwrap();
        assert_eq!(all.len(), 6);
        // Newest first.
        assert!(all[0].id > all[5].id);

        let page = repo
            .list(&TimelineFilter {
                offset: 4,
                limit: 2,
                ..TimelineFilter::default()
            })
            .unwrap();
        assert_eq!(page.len(), 2);

        let photos = repo
            .list(&TimelineFilter {
                kind: Some(TimelineKind::PhotoTaken),
                ..TimelineFilter::default()
            })
            .unwrap();
        assert_eq!(photos.len(), 1);
        assert_eq!(photos[0].kind, TimelineKind::PhotoTaken);

        let hits = repo
            .list(&TimelineFilter {
                query: Some("user 3".to_string()),
                ..TimelineFilter::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].display_name, "User 3");

        assert_eq!(repo.count(&TimelineFilter::default()).unwrap(), 6);
        assert_eq!(
            repo.count(&TimelineFilter {
                kind: Some(TimelineKind::PhotoTaken),
                ..TimelineFilter::default()
            })
            .unwrap(),
            1
        );
    }
}
