//! Session repository: one row per contiguous world visit, opened by a
//! `JoinedWorld` log event and closed by `LeftWorld` (or the next join /
//! app restart). Sessions drive the time-spent statistics.

use rusqlite::{params, Connection, OptionalExtension, Row};
use time::OffsetDateTime;

use crate::{i64_to_ts, lock, ts_to_i64, Conn, StoreError};

/// One world-visit session.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// Row id.
    pub id: i64,
    /// When the visit started.
    pub started_at: OffsetDateTime,
    /// When the visit ended; `None` while the session is still open.
    pub ended_at: Option<OffsetDateTime>,
    /// `wrld_…` world id; empty until the location-bearing join line lands.
    pub world_id: String,
    /// World display name when known.
    pub world_name: String,
    /// Instance spec after `wrld_…:` (may contain `~` parts).
    pub instance_id: String,
    /// Full `wrld_…:instance` location.
    pub location: String,
}

/// Fields for a new session (id and `ended_at` are assigned later).
#[derive(Debug, Clone)]
pub struct NewSession {
    /// When the visit started.
    pub started_at: OffsetDateTime,
    /// `wrld_…` world id.
    pub world_id: String,
    /// World display name when known.
    pub world_name: String,
    /// Instance spec.
    pub instance_id: String,
    /// Full `wrld_…:instance` location.
    pub location: String,
}

impl Default for NewSession {
    fn default() -> Self {
        Self {
            started_at: OffsetDateTime::UNIX_EPOCH,
            world_id: String::new(),
            world_name: String::new(),
            instance_id: String::new(),
            location: String::new(),
        }
    }
}

/// Repository for the `sessions` table.
#[derive(Clone)]
pub struct SessionRepo {
    pub(crate) conn: Conn,
}

impl SessionRepo {
    pub(crate) fn new(conn: Conn) -> Self {
        Self { conn }
    }

    /// Open a new session; returns its id.
    pub fn open(&self, session: &NewSession) -> Result<i64, StoreError> {
        let conn = lock(&self.conn);
        conn.execute(
            "INSERT INTO sessions (started_at, world_id, world_name, instance_id, location)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                ts_to_i64(session.started_at),
                session.world_id,
                session.world_name,
                session.instance_id,
                session.location,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Fill in world details learned after the session opened (the
    /// name/location join lines arrive as separate log events). Only
    /// non-empty fields overwrite.
    pub fn update_world(
        &self,
        id: i64,
        world_id: &str,
        world_name: &str,
        instance_id: &str,
        location: &str,
    ) -> Result<(), StoreError> {
        let conn = lock(&self.conn);
        conn.execute(
            "UPDATE sessions SET
                world_id    = CASE WHEN ?2 != '' THEN ?2 ELSE world_id END,
                world_name  = CASE WHEN ?3 != '' THEN ?3 ELSE world_name END,
                instance_id = CASE WHEN ?4 != '' THEN ?4 ELSE instance_id END,
                location    = CASE WHEN ?5 != '' THEN ?5 ELSE location END
             WHERE id = ?1",
            params![id, world_id, world_name, instance_id, location],
        )?;
        Ok(())
    }

    /// Close a session; a no-op when already closed.
    pub fn close(&self, id: i64, ended_at: OffsetDateTime) -> Result<(), StoreError> {
        let conn = lock(&self.conn);
        conn.execute(
            "UPDATE sessions SET ended_at = ?2 WHERE id = ?1 AND ended_at IS NULL",
            params![id, ts_to_i64(ended_at)],
        )?;
        Ok(())
    }

    /// The currently open session (latest `ended_at IS NULL` row), if any.
    pub fn current(&self) -> Result<Option<Session>, StoreError> {
        let conn = lock(&self.conn);
        conn.query_row(
            &format!("{SESSION_SELECT} WHERE ended_at IS NULL ORDER BY id DESC LIMIT 1"),
            [],
            map_session,
        )
        .optional()
        .map_err(StoreError::Sql)
    }

    /// Close every still-open session at `ended_at` — used at startup to
    /// retire sessions orphaned by a previous app run. `ended_at` is
    /// clamped to `started_at` so clock skew can't produce negative
    /// durations.
    /// Returns the number of sessions closed.
    pub fn close_all_open(&self, ended_at: OffsetDateTime) -> Result<u64, StoreError> {
        let conn = lock(&self.conn);
        let changed = conn.execute(
            "UPDATE sessions SET ended_at = MAX(?1, started_at) WHERE ended_at IS NULL",
            params![ts_to_i64(ended_at)],
        )?;
        Ok(changed as u64)
    }
}

const SESSION_SELECT: &str =
    "SELECT id, started_at, ended_at, world_id, world_name, instance_id, location FROM sessions";

/// All sessions overlapping `[since, until)` (either bound may be absent),
/// ordered by start. Open sessions (`ended_at IS NULL`) always overlap.
/// Used by the stats repository; bounds are unix seconds.
pub(crate) fn sessions_in_range(
    conn: &Connection,
    since: Option<i64>,
    until: Option<i64>,
) -> Result<Vec<Session>, StoreError> {
    // `started_at < until` AND (`ended_at` unknown OR `ended_at > since`).
    // Absent bounds become sentinels that never constrain.
    let rows = conn
        .prepare(&format!(
            "{SESSION_SELECT}
             WHERE started_at < ?1
               AND (ended_at IS NULL OR ended_at > ?2)
             ORDER BY started_at, id"
        ))?
        .query_map(
            params![until.unwrap_or(i64::MAX), since.unwrap_or(i64::MIN)],
            map_session,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn map_session(row: &Row<'_>) -> rusqlite::Result<Session> {
    let ended: Option<i64> = row.get(2)?;
    Ok(Session {
        id: row.get(0)?,
        started_at: i64_to_ts(row.get(1)?),
        ended_at: ended.map(i64_to_ts),
        world_id: row.get(3)?,
        world_name: row.get(4)?,
        instance_id: row.get(5)?,
        location: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use time::Duration;

    #[test]
    fn open_update_close_current() {
        let store = Store::open_in_memory().unwrap();
        let repo = store.sessions();
        let t0 = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_700_000_000);

        let id = repo
            .open(&NewSession {
                started_at: t0,
                world_name: "Test World".to_string(),
                ..NewSession::default()
            })
            .unwrap();
        assert_eq!(repo.current().unwrap().unwrap().id, id);

        // Name-only join line enriches the open session.
        repo.update_world(id, "", "Test World", "", "").unwrap();
        // Location join line fills ids.
        repo.update_world(
            id,
            "wrld_00000000-0000-0000-0000-000000000001",
            "",
            "1234~region(us)",
            "wrld_00000000-0000-0000-0000-000000000001:1234~region(us)",
        )
        .unwrap();
        let current = repo.current().unwrap().unwrap();
        assert_eq!(
            current.world_id,
            "wrld_00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(current.world_name, "Test World");
        assert_eq!(current.instance_id, "1234~region(us)");

        repo.close(id, t0 + Duration::seconds(60)).unwrap();
        assert!(repo.current().unwrap().is_none());
        // Closing twice is a no-op.
        repo.close(id, t0 + Duration::seconds(90)).unwrap();
    }

    #[test]
    fn close_all_open_clamps_end_to_start() {
        let store = Store::open_in_memory().unwrap();
        let repo = store.sessions();
        let t0 = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_700_000_000);
        repo.open(&NewSession {
            started_at: t0,
            ..NewSession::default()
        })
        .unwrap();
        // An end bound before the session start clamps to started_at.
        repo.close_all_open(t0 - Duration::hours(1)).unwrap();
        let conn = lock(&repo.conn);
        let rows = sessions_in_range(&conn, None, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ended_at, Some(t0));
    }

    #[test]
    fn close_all_open_retires_orphans() {
        let store = Store::open_in_memory().unwrap();
        let repo = store.sessions();
        let t0 = OffsetDateTime::UNIX_EPOCH;
        repo.open(&NewSession {
            started_at: t0,
            ..NewSession::default()
        })
        .unwrap();
        repo.open(&NewSession {
            started_at: t0 + Duration::seconds(10),
            ..NewSession::default()
        })
        .unwrap();
        assert_eq!(repo.close_all_open(t0 + Duration::seconds(20)).unwrap(), 2);
        assert!(repo.current().unwrap().is_none());
    }
}
