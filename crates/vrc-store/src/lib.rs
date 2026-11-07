//! SQLite store + migrations (M3).
//!
//! [`Store`] wraps a single `rusqlite` connection behind a mutex and hands
//! out cheap repository handles ([`TimelineRepo`], [`HistoryRepo`],
//! [`SessionRepo`], [`StatsRepo`]). The API is synchronous — `vrc-core`
//! calls it via `spawn_blocking`/`block_in_place` so the async executor is
//! never blocked on disk I/O.
//!
//! Only ids, names, world/instance identifiers and timestamps are
//! persisted — never e-mails, tokens or credentials (RULES.md §4).

mod history;
mod media;
mod sessions;
mod settings;
mod stats;
mod timeline;

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::Connection;
use rusqlite_migration::{Migrations, M};

pub use history::{
    FriendEvent, FriendHistoryEntry, FriendHistoryFilter, HistoryRepo, NewFriendHistoryEntry,
};
pub use media::{MediaFilter, MediaItem, MediaRepo, MediaSignature, ScannedMedia, SyncOutcome};
pub use sessions::{NewSession, Session, SessionRepo};
pub use settings::SettingsRepo;
pub use stats::{FriendTime, Stats, StatsRange, StatsRepo, WorldTime};
pub use timeline::{NewTimelineEntry, TimelineEntry, TimelineFilter, TimelineKind, TimelineRepo};

/// Shared connection handle used by every repository.
pub(crate) type Conn = Arc<Mutex<Connection>>;

/// Rows per page when a filter leaves `limit` at 0.
pub const DEFAULT_PAGE_SIZE: u32 = 100;

/// Errors from store operations. Messages never contain secrets
/// (RULES.md §4) — they are SQLite/migration error text only.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The database file could not be opened or created.
    #[error("failed to open database: {0}")]
    Open(#[source] rusqlite::Error),
    /// A schema migration failed.
    #[error("database migration failed: {0}")]
    Migrate(#[source] rusqlite_migration::Error),
    /// A query or write failed.
    #[error("database error: {0}")]
    Sql(#[from] rusqlite::Error),
    /// Filesystem setup (e.g. creating the data directory) failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Handle to the local SQLite database. Cheaply cloneable.
#[derive(Clone)]
pub struct Store {
    conn: Conn,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Store(..)")
    }
}

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(
            "CREATE TABLE timeline (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            ts          INTEGER NOT NULL,
            kind        TEXT    NOT NULL,
            user_id     TEXT    NOT NULL DEFAULT '',
            display_name TEXT   NOT NULL DEFAULT '',
            world_id    TEXT    NOT NULL DEFAULT '',
            world_name  TEXT    NOT NULL DEFAULT '',
            instance_id TEXT    NOT NULL DEFAULT '',
            location    TEXT    NOT NULL DEFAULT '',
            detail      TEXT    NOT NULL DEFAULT ''
        );
        CREATE INDEX timeline_ts_idx   ON timeline(ts DESC, id DESC);
        CREATE INDEX timeline_kind_idx ON timeline(kind, ts DESC);

        CREATE TABLE friend_history (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            ts           INTEGER NOT NULL,
            user_id      TEXT    NOT NULL,
            display_name TEXT    NOT NULL DEFAULT '',
            event        TEXT    NOT NULL,
            world_id     TEXT    NOT NULL DEFAULT '',
            instance_id  TEXT    NOT NULL DEFAULT '',
            location     TEXT    NOT NULL DEFAULT '',
            platform     TEXT    NOT NULL DEFAULT ''
        );
        CREATE INDEX friend_history_user_idx ON friend_history(user_id, ts DESC, id DESC);
        CREATE INDEX friend_history_ts_idx   ON friend_history(ts DESC, id DESC);

        CREATE TABLE sessions (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            started_at  INTEGER NOT NULL,
            ended_at    INTEGER,
            world_id    TEXT    NOT NULL DEFAULT '',
            world_name  TEXT    NOT NULL DEFAULT '',
            instance_id TEXT    NOT NULL DEFAULT '',
            location    TEXT    NOT NULL DEFAULT ''
        );
        CREATE INDEX sessions_started_idx ON sessions(started_at DESC, id DESC);",
        ),
        M::up(
            "CREATE TABLE media (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            path            TEXT    NOT NULL UNIQUE,
            folder          TEXT    NOT NULL DEFAULT '',
            size            INTEGER NOT NULL DEFAULT 0,
            mtime           INTEGER NOT NULL DEFAULT 0,
            taken_at        INTEGER NOT NULL DEFAULT 0,
            width           INTEGER NOT NULL DEFAULT 0,
            height          INTEGER NOT NULL DEFAULT 0,
            world_id        TEXT    NOT NULL DEFAULT '',
            world_name      TEXT    NOT NULL DEFAULT '',
            is_favorite     INTEGER NOT NULL DEFAULT 0,
            metadata        TEXT    NOT NULL DEFAULT '',
            last_seen_scan  INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX media_taken_idx   ON media(taken_at DESC, id DESC);
        CREATE INDEX media_folder_idx  ON media(folder, taken_at DESC);
        CREATE INDEX media_world_idx   ON media(world_id, taken_at DESC);

        CREATE TABLE settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
        ),
    ])
}

impl Store {
    /// Open (creating if needed) the database at `path`, running pending
    /// migrations. Missing parent directories are created.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path).map_err(StoreError::Open)?;
        Self::from_connection(conn)
    }

    /// An in-memory database for tests and headless use.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory().map_err(StoreError::Open)?;
        Self::from_connection(conn)
    }

    fn from_connection(mut conn: Connection) -> Result<Self, StoreError> {
        // WAL keeps reads responsive while the tailer writes; a no-op for
        // :memory: databases.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(StoreError::Open)?;
        conn.pragma_update(None, "busy_timeout", 5_000)
            .map_err(StoreError::Open)?;
        migrations()
            .to_latest(&mut conn)
            .map_err(StoreError::Migrate)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Personal timeline rows (world joins/leaves, player joins/leaves,
    /// photos) parsed from VRChat logs.
    pub fn timeline(&self) -> TimelineRepo {
        TimelineRepo::new(self.conn.clone())
    }

    /// Friend presence/location history from pipeline events.
    pub fn history(&self) -> HistoryRepo {
        HistoryRepo::new(self.conn.clone())
    }

    /// World-visit sessions with start/end times (time-spent stats).
    pub fn sessions(&self) -> SessionRepo {
        SessionRepo::new(self.conn.clone())
    }

    /// Aggregated time-spent statistics.
    pub fn stats(&self) -> StatsRepo {
        StatsRepo::new(self.conn.clone())
    }

    /// Indexed photo library (M6).
    pub fn media(&self) -> MediaRepo {
        MediaRepo::new(self.conn.clone())
    }

    /// Key/value app settings (M6).
    pub fn settings(&self) -> SettingsRepo {
        SettingsRepo::new(self.conn.clone())
    }
}

/// Lock the shared connection; a poisoned mutex still yields the guard —
/// a panicking writer must not wedge the whole store.
pub(crate) fn lock(conn: &Conn) -> MutexGuard<'_, Connection> {
    conn.lock().unwrap_or_else(|e| e.into_inner())
}

/// `ts` columns are unix seconds; `OffsetDateTime` conversions live here so
/// repositories stay uniform.
pub(crate) fn ts_to_i64(ts: time::OffsetDateTime) -> i64 {
    ts.unix_timestamp()
}

pub(crate) fn i64_to_ts(ts: i64) -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp(ts).unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    #[test]
    fn migrations_create_all_tables() {
        let store = Store::open_in_memory().unwrap();
        let conn = lock(&store.conn);
        for table in [
            "timeline",
            "friend_history",
            "sessions",
            "media",
            "settings",
        ] {
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name=?1",
                    params![table],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(exists, "missing table {table}");
        }
    }

    #[test]
    fn open_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("deeper").join("data.db");
        let store = Store::open(&path).unwrap();
        drop(store);
        assert!(path.exists());
    }
}
