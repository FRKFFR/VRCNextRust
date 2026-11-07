//! Media repository (M6): indexed photo files from the configured scan
//! folders. Rows are keyed by absolute file path; scanner syncs stamp
//! `last_seen_scan` so vanished files are dropped per folder. `taken_at`
//! is the display/sort timestamp — the file mtime by default, refined to
//! the log-event instant by [`MediaRepo::link_photo`]. Favorites and
//! world links are local-only state the scanner never overwrites.

use std::collections::HashMap;

use rusqlite::{params, params_from_iter, Row};
use time::OffsetDateTime;

use crate::{i64_to_ts, lock, ts_to_i64, Conn, StoreError, DEFAULT_PAGE_SIZE};

/// One indexed photo file.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaItem {
    /// Row id.
    pub id: i64,
    /// Absolute file path (unique key).
    pub path: String,
    /// Scan-root folder this file was found under.
    pub folder: String,
    /// File size in bytes.
    pub size: i64,
    /// File modification time.
    pub mtime: OffsetDateTime,
    /// When the photo was taken (log-event instant when known, else mtime).
    pub taken_at: OffsetDateTime,
    /// Pixel width (0 until probed).
    pub width: u32,
    /// Pixel height (0 until probed).
    pub height: u32,
    /// `wrld_…` world id when linked via a `PhotoTaken` log event.
    pub world_id: String,
    /// World display name when known.
    pub world_name: String,
    /// Local favorite flag.
    pub is_favorite: bool,
    /// JSON object of PNG text-chunk metadata (`{}` when absent).
    pub metadata: String,
}

/// A file discovered by the scanner, with probe results already filled in.
#[derive(Debug, Clone)]
pub struct ScannedMedia {
    /// Absolute file path.
    pub path: String,
    /// File size in bytes.
    pub size: i64,
    /// File modification time (unix seconds).
    pub mtime: i64,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Taken timestamp (unix seconds; normally `mtime`).
    pub taken_at: i64,
    /// JSON object of PNG text-chunk metadata.
    pub metadata: String,
}

/// Filters + paging for [`MediaRepo::list`]/[`MediaRepo::count`].
#[derive(Debug, Clone, Default)]
pub struct MediaFilter {
    /// Restrict to files found under this scan root.
    pub folder: Option<String>,
    /// Only favorited photos.
    pub favorites_only: bool,
    /// Only photos linked to this `wrld_…` id.
    pub world_id: Option<String>,
    /// Case-insensitive substring matched against path and world name.
    pub query: Option<String>,
    /// Only photos taken at/after this instant.
    pub since: Option<OffsetDateTime>,
    /// Only photos taken before this instant.
    pub until: Option<OffsetDateTime>,
    /// Rows to skip (0 = first page).
    pub offset: u32,
    /// Page size; `0` uses [`DEFAULT_PAGE_SIZE`].
    pub limit: u32,
}

/// Scan-relevant fields of an existing row, used by the scanner to skip
/// re-probing unchanged files.
#[derive(Debug, Clone)]
pub struct MediaSignature {
    /// File size in bytes.
    pub size: i64,
    /// File modification time (unix seconds).
    pub mtime: i64,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Stored taken timestamp (unix seconds).
    pub taken_at: i64,
    /// Stored JSON metadata.
    pub metadata: String,
}

/// Counts from one [`MediaRepo::sync_folder`] run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncOutcome {
    /// Rows inserted or updated.
    pub upserted: u64,
    /// Rows deleted because their files vanished.
    pub removed: u64,
}

/// Repository for the `media` table.
#[derive(Clone)]
pub struct MediaRepo {
    pub(crate) conn: Conn,
}

const MEDIA_SELECT: &str = "SELECT id, path, folder, size, mtime, taken_at, width, height,
        world_id, world_name, is_favorite, metadata FROM media";

impl MediaRepo {
    pub(crate) fn new(conn: Conn) -> Self {
        Self { conn }
    }

    /// Replace the contents of one scan folder: upsert every scanned item
    /// stamped with `scan_id`, then delete rows of that folder the scan
    /// did not see. User state (`is_favorite`, `world_*`, `taken_at`)
    /// survives upserts; `taken_at` resets only when the file itself
    /// changed (size/mtime differ and the row was scan-populated).
    pub fn sync_folder(
        &self,
        scan_id: i64,
        folder: &str,
        items: &[ScannedMedia],
    ) -> Result<SyncOutcome, StoreError> {
        let mut conn = lock(&self.conn);
        let tx = conn.transaction()?;
        let mut upserted = 0u64;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO media
                    (path, folder, size, mtime, taken_at, width, height,
                     metadata, last_seen_scan)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(path) DO UPDATE SET
                    folder         = excluded.folder,
                    size           = excluded.size,
                    mtime          = excluded.mtime,
                    width          = excluded.width,
                    height         = excluded.height,
                    metadata       = excluded.metadata,
                    last_seen_scan = excluded.last_seen_scan,
                    taken_at = CASE
                        WHEN media.size > 0
                         AND (media.size != excluded.size
                           OR media.mtime != excluded.mtime)
                        THEN excluded.taken_at
                        ELSE media.taken_at
                    END",
            )?;
            for item in items {
                stmt.execute(params![
                    item.path,
                    folder,
                    item.size,
                    item.mtime,
                    item.taken_at,
                    i64::from(item.width),
                    i64::from(item.height),
                    item.metadata,
                    scan_id,
                ])?;
                upserted += 1;
            }
        }
        let removed = tx.execute(
            "DELETE FROM media WHERE folder = ?1 AND last_seen_scan != ?2",
            params![folder, scan_id],
        )? as u64;
        tx.commit()?;
        Ok(SyncOutcome { upserted, removed })
    }

    /// Link a `PhotoTaken` log event to the media library: upsert a stub
    /// row when the file has not been scanned yet, always refresh
    /// `taken_at`, and attach the world of the session covering `ts`.
    /// `world_id`/`world_name` only overwrite when non-empty.
    pub fn link_photo(
        &self,
        path: &str,
        ts: OffsetDateTime,
        folder: &str,
        world_id: &str,
        world_name: &str,
    ) -> Result<(), StoreError> {
        let conn = lock(&self.conn);
        conn.execute(
            "INSERT INTO media
                (path, folder, size, mtime, taken_at, world_id, world_name,
                 last_seen_scan)
             VALUES (?1, ?2, 0, 0, ?3, ?4, ?5, -1)
             ON CONFLICT(path) DO UPDATE SET
                taken_at   = excluded.taken_at,
                world_id   = CASE WHEN excluded.world_id != ''
                                  THEN excluded.world_id ELSE media.world_id END,
                world_name = CASE WHEN excluded.world_name != ''
                                  THEN excluded.world_name ELSE media.world_name END",
            params![path, folder, ts_to_i64(ts), world_id, world_name,],
        )?;
        Ok(())
    }

    /// `path → MediaSignature` for every row — the scanner's
    /// incremental-change input. Unchanged files reuse the stored probe
    /// data so resyncs never re-read image headers.
    pub fn signatures(&self) -> Result<HashMap<String, MediaSignature>, StoreError> {
        let conn = lock(&self.conn);
        let mut stmt =
            conn.prepare("SELECT path, size, mtime, taken_at, width, height, metadata FROM media")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    MediaSignature {
                        size: row.get(1)?,
                        mtime: row.get(2)?,
                        taken_at: row.get(3)?,
                        width: row.get::<_, i64>(4)? as u32,
                        height: row.get::<_, i64>(5)? as u32,
                        metadata: row.get(6)?,
                    },
                ))
            })?
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(rows)
    }

    /// Newest-first page of media matching `filter` (ordered by
    /// `taken_at`, the photo's own timestamp).
    pub fn list(&self, filter: &MediaFilter) -> Result<Vec<MediaItem>, StoreError> {
        let conn = lock(&self.conn);
        let (where_sql, binds) = filter_clauses(filter);
        let sql = format!(
            "{MEDIA_SELECT} {where_sql}
             ORDER BY taken_at DESC, id DESC
             LIMIT ? OFFSET ?"
        );
        let mut stmt = conn.prepare(&sql)?;
        let limit = effective_limit(filter.limit);
        let offset = i64::from(filter.offset);
        let rows = stmt.query_map(
            params_from_iter(params_from_binds(binds, limit, offset)),
            map_media,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::Sql)
    }

    /// Total rows matching `filter` (ignores `offset`/`limit`).
    pub fn count(&self, filter: &MediaFilter) -> Result<u64, StoreError> {
        let conn = lock(&self.conn);
        let (where_sql, binds) = filter_clauses(filter);
        let sql = format!("SELECT COUNT(*) FROM media {where_sql}");
        let mut stmt = conn.prepare(&sql)?;
        let count: i64 = stmt.query_row(params_from_iter(params_from_binds_count(binds)), |r| {
            r.get(0)
        })?;
        Ok(count.max(0) as u64)
    }

    /// Remove every index row under `folder` — used when a custom folder
    /// is unregistered. Photo files themselves are never touched.
    pub fn delete_folder(&self, folder: &str) -> Result<u64, StoreError> {
        let conn = lock(&self.conn);
        let changed = conn.execute("DELETE FROM media WHERE folder = ?1", params![folder])?;
        Ok(changed as u64)
    }

    /// Set the local favorite flag; returns whether a row matched.
    pub fn set_favorite(&self, id: i64, favorite: bool) -> Result<bool, StoreError> {
        let conn = lock(&self.conn);
        let changed = conn.execute(
            "UPDATE media SET is_favorite = ?2 WHERE id = ?1",
            params![id, i64::from(favorite)],
        )?;
        Ok(changed > 0)
    }
}

fn effective_limit(limit: u32) -> i64 {
    i64::from(if limit == 0 { DEFAULT_PAGE_SIZE } else { limit })
}

/// WHERE fragment + positional binds for the optional filters.
fn filter_clauses(filter: &MediaFilter) -> (String, Vec<rusqlite::types::Value>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut binds: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(folder) = filter.folder.as_deref().filter(|f| !f.is_empty()) {
        clauses.push("folder = ?".to_string());
        binds.push(folder.to_string().into());
    }
    if filter.favorites_only {
        clauses.push("is_favorite = 1".to_string());
    }
    if let Some(world_id) = filter.world_id.as_deref().filter(|w| !w.is_empty()) {
        clauses.push("world_id = ?".to_string());
        binds.push(world_id.to_string().into());
    }
    if let Some(since) = filter.since {
        clauses.push("taken_at >= ?".to_string());
        binds.push(ts_to_i64(since).into());
    }
    if let Some(until) = filter.until {
        clauses.push("taken_at < ?".to_string());
        binds.push(ts_to_i64(until).into());
    }
    if filter
        .query
        .as_deref()
        .is_some_and(|q| !q.trim().is_empty())
    {
        clauses.push("instr(lower(path || char(31) || world_name), lower(?)) > 0".to_string());
        binds.push(filter.query.clone().unwrap_or_default().into());
    }
    let sql = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    (sql, binds)
}

fn params_from_binds(
    binds: Vec<rusqlite::types::Value>,
    limit: i64,
    offset: i64,
) -> Vec<rusqlite::types::Value> {
    let mut values = binds;
    values.push(limit.into());
    values.push(offset.into());
    values
}

fn params_from_binds_count(binds: Vec<rusqlite::types::Value>) -> Vec<rusqlite::types::Value> {
    binds
}

fn map_media(row: &Row<'_>) -> rusqlite::Result<MediaItem> {
    Ok(MediaItem {
        id: row.get(0)?,
        path: row.get(1)?,
        folder: row.get(2)?,
        size: row.get(3)?,
        mtime: i64_to_ts(row.get(4)?),
        taken_at: i64_to_ts(row.get(5)?),
        width: row.get::<_, i64>(6)? as u32,
        height: row.get::<_, i64>(7)? as u32,
        world_id: row.get(8)?,
        world_name: row.get(9)?,
        is_favorite: row.get::<_, i64>(10)? != 0,
        metadata: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use time::Duration;

    fn scanned(path: &str, size: i64, mtime: i64) -> ScannedMedia {
        ScannedMedia {
            path: path.to_string(),
            size,
            mtime,
            width: 100,
            height: 50,
            taken_at: mtime,
            metadata: "{}".to_string(),
        }
    }

    #[test]
    fn sync_upserts_and_removes_stale() {
        let store = Store::open_in_memory().unwrap();
        let repo = store.media();
        let t0 = 1_700_000_000i64;
        let items = vec![
            scanned("C:/photos/a.png", 10, t0),
            scanned("C:/photos/b.jpg", 20, t0 + 1),
        ];
        let out = repo.sync_folder(1, "C:/photos", &items).unwrap();
        assert_eq!(out.upserted, 2);
        assert_eq!(out.removed, 0);

        // Re-sync with one file gone and one changed: a.png unchanged is
        // kept, b.jpg removed, c.png added.
        let items2 = vec![
            scanned("C:/photos/a.png", 10, t0),
            scanned("C:/photos/c.png", 30, t0 + 2),
        ];
        let out = repo.sync_folder(2, "C:/photos", &items2).unwrap();
        assert_eq!(out.upserted, 2);
        assert_eq!(out.removed, 1);
        let all = repo.list(&MediaFilter::default()).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|m| m.path.ends_with("a.png")));
        assert!(all.iter().any(|m| m.path.ends_with("c.png")));
    }

    #[test]
    fn sync_preserves_favorite_world_and_taken_at() {
        let store = Store::open_in_memory().unwrap();
        let repo = store.media();
        let t0 = 1_700_000_000i64;
        repo.sync_folder(1, "C:/photos", &[scanned("C:/photos/a.png", 10, t0)])
            .unwrap();
        let item = &repo.list(&MediaFilter::default()).unwrap()[0];
        repo.set_favorite(item.id, true).unwrap();
        let taken = OffsetDateTime::UNIX_EPOCH + Duration::seconds(t0 - 60);
        repo.link_photo(
            "C:/photos/a.png",
            taken,
            "C:/photos",
            "wrld_1",
            "Test World",
        )
        .unwrap();

        // Rescan with identical file data: user state must survive.
        repo.sync_folder(2, "C:/photos", &[scanned("C:/photos/a.png", 10, t0)])
            .unwrap();
        let item = &repo.list(&MediaFilter::default()).unwrap()[0];
        assert!(item.is_favorite);
        assert_eq!(item.taken_at, taken);
        assert_eq!(item.world_id, "wrld_1");
        assert_eq!(item.world_name, "Test World");
    }

    #[test]
    fn link_photo_creates_stub_then_scan_fills_it() {
        let store = Store::open_in_memory().unwrap();
        let repo = store.media();
        let ts = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_700_000_100);
        repo.link_photo("C:/photos/new.png", ts, "C:/photos", "wrld_9", "")
            .unwrap();
        let item = &repo.list(&MediaFilter::default()).unwrap()[0];
        assert_eq!(item.path, "C:/photos/new.png");
        assert_eq!(item.taken_at, ts);
        assert_eq!(item.world_id, "wrld_9");
        assert_eq!(item.size, 0);

        // The later scan fills size/dims but keeps the linked taken_at
        // (the stub's size=0 sentinel marks "not scan-populated").
        repo.sync_folder(
            1,
            "C:/photos",
            &[scanned("C:/photos/new.png", 42, 1_700_000_000)],
        )
        .unwrap();
        let item = &repo.list(&MediaFilter::default()).unwrap()[0];
        assert_eq!(item.size, 42);
        assert_eq!(item.taken_at, ts);
        assert_eq!(item.world_id, "wrld_9");
    }

    #[test]
    fn filters_and_paging() {
        let store = Store::open_in_memory().unwrap();
        let repo = store.media();
        let t0 = 1_700_000_000i64;
        repo.sync_folder(
            1,
            "C:/photos",
            &[
                scanned("C:/photos/a.png", 1, t0),
                scanned("C:/photos/b.png", 2, t0 + 10),
                scanned("C:/other/c.png", 3, t0 + 20),
            ],
        )
        .unwrap();
        // c.png lives under a different folder root.
        repo.sync_folder(1, "C:/other", &[scanned("C:/other/c.png", 3, t0 + 20)])
            .unwrap();

        let fav = repo.list(&MediaFilter::default()).unwrap()[0].clone();
        repo.set_favorite(fav.id, true).unwrap();

        let favs = repo
            .list(&MediaFilter {
                favorites_only: true,
                ..MediaFilter::default()
            })
            .unwrap();
        assert_eq!(favs.len(), 1);
        assert!(favs[0].is_favorite);

        let folder = repo
            .list(&MediaFilter {
                folder: Some("C:/other".to_string()),
                ..MediaFilter::default()
            })
            .unwrap();
        assert_eq!(folder.len(), 1);
        assert!(folder[0].path.contains("other"));

        let page = repo
            .list(&MediaFilter {
                offset: 1,
                limit: 1,
                ..MediaFilter::default()
            })
            .unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(repo.count(&MediaFilter::default()).unwrap(), 3);

        let ranged = repo
            .list(&MediaFilter {
                since: Some(OffsetDateTime::UNIX_EPOCH + Duration::seconds(t0 + 15)),
                ..MediaFilter::default()
            })
            .unwrap();
        assert_eq!(ranged.len(), 1);
        assert!(ranged[0].path.contains("c.png"));
    }
}
