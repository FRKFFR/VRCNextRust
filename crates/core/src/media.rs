//! M6 media library: scan-folder configuration, the background scanner,
//! PNG text-chunk metadata and the remote-image host allowlist.
//!
//! Everything here is local-file work — it runs on `spawn_blocking` /
//! detached tasks, never on the UI thread, and never writes to photo
//! files (RULES.md §3/§7). Secrets are not involved.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use serde_json::{Map, Value};
use time::OffsetDateTime;
use tokio::sync::{broadcast, oneshot};
use vrc_store::{MediaFilter, MediaSignature, ScannedMedia};

use crate::app::{AppEvent, StateHandle};
use crate::local::StoreOp;
use crate::state::{MediaFolderInfo, MediaScanInfo};

/// Settings keys persisted in the `settings` table.
pub const SETTING_MEDIA_FOLDERS: &str = "media.folders";
pub const SETTING_MEDIA_BLUR: &str = "media.blur_until_hover";

/// Rows per media page.
pub const MEDIA_PAGE_SIZE: u32 = 60;

/// Deepest directory level the scanner descends into.
pub const SCAN_DEPTH_CAP: usize = 16;

/// How much of a PNG is read while looking for text chunks (metadata
/// always sits in the first chunks; 1 MiB is generous).
const PNG_META_READ_CAP: u64 = 1024 * 1024;

/// PNG signature bytes.
const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Largest single chunk payload considered while walking PNG chunks.
const PNG_MAX_CHUNK: u64 = 64 * 1024 * 1024;

/// Cap on parsed text entries and on the emitted metadata JSON.
const PNG_MAX_ENTRIES: usize = 64;
const PNG_MAX_VALUE: usize = 4096;
const PNG_MAX_JSON: usize = 16 * 1024;

/// The platform's VRChat Pictures folder (`<Pictures>/VRChat`).
pub fn default_media_dir() -> Option<PathBuf> {
    let dirs = directories::UserDirs::new()?;
    if let Some(pictures) = dirs.picture_dir() {
        return Some(pictures.join("VRChat"));
    }
    Some(dirs.home_dir().join("Pictures").join("VRChat"))
}

/// Parse the persisted `media.folders` JSON array into folder strings.
pub fn parse_custom_folders(json: Option<&str>) -> Vec<String> {
    let Some(raw) = json else { return Vec::new() };
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

/// Serialize custom folder strings for [`SETTING_MEDIA_FOLDERS`].
pub fn folders_json(folders: &[String]) -> String {
    serde_json::to_string(folders).unwrap_or_else(|_| "[]".to_string())
}

/// Effective scan folders: the platform default first, then user-added
/// custom folders, deduplicated by exact path string.
pub fn effective_folders(custom: &[String]) -> Vec<MediaFolderInfo> {
    let mut out: Vec<MediaFolderInfo> = Vec::new();
    if let Some(dir) = default_media_dir() {
        out.push(MediaFolderInfo {
            path: dir.to_string_lossy().into_owned(),
            custom: false,
        });
    }
    for folder in custom {
        if !out.iter().any(|info| info.path == *folder) {
            out.push(MediaFolderInfo {
                path: folder.clone(),
                custom: true,
            });
        }
    }
    out
}

/// Whether `path` is a `*.png`/`*.jpg`/`*.jpeg` file (case-insensitive).
pub fn is_media_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"))
}

/// Whether a remote image URL may be fetched: `https` only and the host
/// is a documented VRChat image host. Loopback `http` is allowed only in
/// `cfg(test)` builds for wiremock fixtures.
pub fn is_allowed_image_url(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    match url.scheme() {
        "https" => {}
        "http" if cfg!(test) => {
            return matches!(
                url.host_str(),
                Some("127.0.0.1") | Some("localhost") | Some("::1")
            )
        }
        _ => return false,
    }
    let Some(host) = url.host_str().map(|h| h.to_ascii_lowercase()) else {
        return false;
    };
    is_vrchat_image_host(&host)
}

/// The documented VRChat image hosts: `api.vrchat.cloud`, `api.vrchat.com`,
/// `cdn.vrchat.com` plus their subdomains.
fn is_vrchat_image_host(host: &str) -> bool {
    ["api.vrchat.cloud", "api.vrchat.com", "cdn.vrchat.com"]
        .iter()
        .any(|base| host == *base || host.ends_with(&format!(".{base}")))
}

/// Stable cache/filter key for a [`MediaFilter`].
pub fn media_filter_key(filter: &MediaFilter) -> String {
    let folder = filter.folder.as_deref().unwrap_or("");
    let world = filter.world_id.as_deref().unwrap_or("");
    let query = filter.query.as_deref().unwrap_or("");
    let since = filter
        .since
        .map(|t| t.unix_timestamp())
        .map_or_else(String::new, |t| t.to_string());
    let until = filter
        .until
        .map(|t| t.unix_timestamp())
        .map_or_else(String::new, |t| t.to_string());
    let fav = filter.favorites_only;
    format!("folder={folder}\x1ffav={fav}\x1fworld={world}\x1fq={query}\x1fsince={since}\x1funtil={until}")
}

/// Parse PNG `tEXt`/`iTXt` chunks into a JSON object string. Tolerant of
/// truncated or malformed input: parsing stops at the first structural
/// error and whatever was collected is returned (`"{}"` when empty).
/// CRCs are not verified — this is metadata extraction, not validation.
pub fn png_text_metadata(bytes: &[u8]) -> String {
    if !bytes.starts_with(&PNG_SIG) {
        return "{}".to_string();
    }
    let mut entries: Vec<(String, String)> = Vec::new();
    let mut pos = PNG_SIG.len();
    while entries.len() < PNG_MAX_ENTRIES {
        // length(4) + type(4) + data + crc(4)
        let header = match bytes.get(pos..pos + 8) {
            Some(h) => h,
            None => break,
        };
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
        let chunk_type = &header[4..8];
        if len > PNG_MAX_CHUNK {
            break;
        }
        let data_start = pos + 8;
        let data_end = match data_start.checked_add(len as usize) {
            Some(end) if end + 4 <= bytes.len() => end,
            _ => break,
        };
        let data = &bytes[data_start..data_end];
        match chunk_type {
            b"tEXt" => {
                if let Some(pair) = parse_text_chunk(data) {
                    entries.push(pair);
                }
            }
            b"iTXt" => {
                if let Some(pair) = parse_itxt_chunk(data) {
                    entries.push(pair);
                }
            }
            _ => {}
        }
        pos = data_end + 4; // skip CRC
                            // Text chunks after IDAT are rare and not worth scanning
                            // megabytes of compressed data for.
        if chunk_type == b"IDAT" {
            break;
        }
    }
    let mut map = Map::new();
    for (key, value) in entries {
        let value: String = value.chars().take(PNG_MAX_VALUE).collect();
        map.insert(key, Value::String(value));
    }
    let json = Value::Object(map).to_string();
    if json.len() > PNG_MAX_JSON {
        return "{}".to_string();
    }
    json
}

/// `tEXt`: `keyword\0text` (latin-1 → lossy UTF-8).
fn parse_text_chunk(data: &[u8]) -> Option<(String, String)> {
    let nul = data.iter().position(|b| *b == 0)?;
    let key = String::from_utf8_lossy(&data[..nul]).into_owned();
    let value = String::from_utf8_lossy(&data[nul + 1..]).into_owned();
    valid_chunk(&key, &value)
}

/// `iTXt`: `keyword\0 compressionFlag(1) compressionMethod(1)
/// languageTag\0 translatedKeyword\0 text`. Only uncompressed text is
/// read (compression flag must be 0).
fn parse_itxt_chunk(data: &[u8]) -> Option<(String, String)> {
    let nul = data.iter().position(|b| *b == 0)?;
    let key = String::from_utf8_lossy(&data[..nul]).into_owned();
    let rest = &data[nul + 1..];
    if rest.len() < 2 || rest[0] != 0 {
        return None;
    }
    let rest = &rest[2..];
    let lang_end = rest.iter().position(|b| *b == 0)?;
    let rest = &rest[lang_end + 1..];
    let trans_end = rest.iter().position(|b| *b == 0)?;
    let value = String::from_utf8_lossy(&rest[trans_end + 1..]).into_owned();
    valid_chunk(&key, &value)
}

fn valid_chunk(key: &str, value: &str) -> Option<(String, String)> {
    if key.is_empty() || key.len() > 80 || value.is_empty() {
        return None;
    }
    Some((key.to_string(), value.to_string()))
}

/// Outcome of one full scan, surfaced in `AppState.media_last_scan`.
#[derive(Debug, Clone)]
pub struct ScanOutcome {
    /// Folders that existed and were walked.
    pub folders: u32,
    /// Image files seen across all folders.
    pub files: u64,
    /// Rows upserted.
    pub upserted: u64,
    /// Stale rows removed.
    pub removed: u64,
    /// Files that failed probing (still indexed with 0×0).
    pub errors: u64,
    /// When the scan finished.
    pub finished_at: OffsetDateTime,
}

/// Run one scan of every configured folder, then update state and emit
/// `AppEvent::MediaScanFinished`. Runs detached from the command loop.
pub(crate) async fn run_media_scan(
    store_tx: std::sync::mpsc::Sender<StoreOp>,
    state: StateHandle,
    events: broadcast::Sender<AppEvent>,
) -> ScanOutcome {
    let folders: Vec<String> = state
        .read()
        .media_folders
        .iter()
        .map(|f| f.path.clone())
        .collect();
    let signatures = Arc::new(query_signatures(&store_tx).await.unwrap_or_default());
    let scan_id = OffsetDateTime::now_utc().unix_timestamp();
    let mut outcome = ScanOutcome {
        folders: 0,
        files: 0,
        upserted: 0,
        removed: 0,
        errors: 0,
        finished_at: OffsetDateTime::UNIX_EPOCH,
    };

    for folder in folders {
        let root = PathBuf::from(&folder);
        if !root.is_dir() {
            continue;
        }
        outcome.folders += 1;
        let sigs = signatures.clone();
        let walked = tokio::task::spawn_blocking(move || scan_folder(&root, &sigs)).await;
        match walked {
            Ok(Ok(items)) => {
                outcome.files += items.len() as u64;
                outcome.errors += items.iter().filter(|i| i.width == 0).count() as u64;
                let (reply_tx, reply_rx) = oneshot::channel();
                if store_tx
                    .send(StoreOp::SyncMediaFolder {
                        scan_id,
                        folder: folder.clone(),
                        items,
                        reply: reply_tx,
                    })
                    .is_ok()
                {
                    if let Ok(Ok(sync)) = reply_rx.await {
                        outcome.upserted += sync.upserted;
                        outcome.removed += sync.removed;
                    }
                }
            }
            Ok(Err(e)) => {
                outcome.errors += 1;
                tracing::debug!("media scan of {folder} failed: {e}");
            }
            Err(e) => {
                outcome.errors += 1;
                tracing::debug!("media scan task for {folder} failed: {e}");
            }
        }
    }
    outcome.finished_at = OffsetDateTime::now_utc();

    {
        let mut guard = state.write();
        guard.media_scanning = false;
        guard.media_last_scan = Some(MediaScanInfo {
            finished_at: outcome.finished_at,
            folders: outcome.folders,
            files: outcome.files,
            upserted: outcome.upserted,
            removed: outcome.removed,
            errors: outcome.errors,
        });
    }
    let _ = events.send(AppEvent::MediaScanFinished);
    outcome
}

/// Blocking folder walk + probe. Returns `Err` only when the root itself
/// cannot be read; individual unreadable entries are skipped.
fn scan_folder(
    root: &Path,
    signatures: &HashMap<String, MediaSignature>,
) -> std::io::Result<Vec<ScannedMedia>> {
    let mut files = Vec::new();
    let mut visited = HashSet::new();
    walk_dir(root, 0, &mut visited, &mut files)?;
    Ok(files
        .iter()
        .map(|file| scanned_item(file, signatures))
        .collect())
}

struct WalkedFile {
    path: PathBuf,
    size: i64,
    mtime: i64,
}

/// Recursive walk honoring [`SCAN_DEPTH_CAP`]. Directory symlinks are
/// never followed (`file_type` does not traverse links); the canonical
/// `visited` set additionally guards against hard-linked dir cycles.
fn walk_dir(
    dir: &Path,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<WalkedFile>,
) -> std::io::Result<()> {
    if depth > SCAN_DEPTH_CAP {
        return Ok(());
    }
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    if !visited.insert(canonical) {
        return Ok(());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        let path = entry.path();
        if file_type.is_dir() {
            let _ = walk_dir(&path, depth + 1, visited, out);
        } else if file_type.is_file() && is_media_file(&path) {
            if let Ok(meta) = entry.metadata() {
                out.push(WalkedFile {
                    path,
                    size: meta.len() as i64,
                    mtime: meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0),
                });
            }
        }
    }
    Ok(())
}

/// Build the store row for one walked file. Files whose
/// `(path,size,mtime)` signature is unchanged reuse the stored probe
/// data; new/changed files are probed here.
fn scanned_item(file: &WalkedFile, signatures: &HashMap<String, MediaSignature>) -> ScannedMedia {
    let path = file.path.to_string_lossy().into_owned();
    if let Some(sig) = signatures.get(&path) {
        // Rows with size == 0 are `link_photo` stubs — always re-probe.
        if sig.size > 0 && sig.size == file.size && sig.mtime == file.mtime {
            return ScannedMedia {
                path,
                size: file.size,
                mtime: file.mtime,
                width: sig.width,
                height: sig.height,
                taken_at: sig.taken_at,
                metadata: sig.metadata.clone(),
            };
        }
    }
    let (width, height) = probe_dimensions(&file.path);
    let metadata = if file
        .path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("png"))
    {
        probe_png_metadata(&file.path)
    } else {
        "{}".to_string()
    };
    ScannedMedia {
        path,
        size: file.size,
        mtime: file.mtime,
        width,
        height,
        taken_at: file.mtime,
        metadata,
    }
}

/// Read image dimensions without decoding pixels (`into_dimensions`
/// only parses headers). `(0, 0)` on any failure — the file is still
/// indexed so a corrupt/unsupported file doesn't vanish from the list.
fn probe_dimensions(path: &Path) -> (u32, u32) {
    let Ok(reader) = image::ImageReader::open(path) else {
        return (0, 0);
    };
    let Ok(reader) = reader.with_guessed_format() else {
        return (0, 0);
    };
    reader.into_dimensions().unwrap_or((0, 0))
}

/// Read up to [`PNG_META_READ_CAP`] bytes of a PNG and extract its
/// `tEXt`/`iTXt` chunks.
fn probe_png_metadata(path: &Path) -> String {
    let Ok(file) = std::fs::File::open(path) else {
        return "{}".to_string();
    };
    let mut buf = Vec::new();
    if file.take(PNG_META_READ_CAP).read_to_end(&mut buf).is_err() {
        return "{}".to_string();
    }
    png_text_metadata(&buf)
}

async fn query_signatures(
    store_tx: &std::sync::mpsc::Sender<StoreOp>,
) -> Option<HashMap<String, MediaSignature>> {
    let (reply_tx, reply_rx) = oneshot::channel();
    store_tx
        .send(StoreOp::QueryMediaSignatures { reply: reply_tx })
        .ok()?;
    match reply_rx.await {
        Ok(Ok(map)) => Some(map),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_media_file_accepts_only_images() {
        assert!(is_media_file(Path::new("a/b/shot.PNG")));
        assert!(is_media_file(Path::new("a.jpg")));
        assert!(is_media_file(Path::new("a.jpeg")));
        assert!(!is_media_file(Path::new("a.gif")));
        assert!(!is_media_file(Path::new("a.png.txt")));
        assert!(!is_media_file(Path::new("noext")));
    }

    #[test]
    fn image_url_allowlist() {
        assert!(is_allowed_image_url(
            "https://api.vrchat.cloud/api/1/image/file_1/1/file"
        ));
        assert!(is_allowed_image_url("https://cdn.vrchat.com/x.png"));
        assert!(is_allowed_image_url("https://foo.api.vrchat.cloud/x"));
        assert!(!is_allowed_image_url("http://api.vrchat.cloud/x.png"));
        assert!(!is_allowed_image_url("https://evil.com/api.vrchat.cloud/x"));
        assert!(!is_allowed_image_url("https://vrchat.cloud.evil.com/x"));
        assert!(!is_allowed_image_url("not a url"));
    }

    #[test]
    fn png_metadata_parses_text_chunks() {
        // Minimal PNG: signature + tEXt chunk + tail.
        let mut png = PNG_SIG.to_vec();
        let payload = b"Comment\0hello world";
        png.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        png.extend_from_slice(b"tEXt");
        png.extend_from_slice(payload);
        png.extend_from_slice(&[0, 0, 0, 0]); // fake CRC
        let json = png_text_metadata(&png);
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["Comment"], "hello world");
    }

    #[test]
    fn png_metadata_handles_garbage() {
        assert_eq!(png_text_metadata(b""), "{}");
        assert_eq!(png_text_metadata(b"not a png"), "{}");
        let mut png = PNG_SIG.to_vec();
        png.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // absurd length
        assert_eq!(png_text_metadata(&png), "{}");
    }

    #[test]
    fn folders_json_roundtrip() {
        let folders = vec!["C:\\Photos".to_string(), "D:\\More".to_string()];
        let json = folders_json(&folders);
        assert_eq!(parse_custom_folders(Some(&json)), folders);
        assert!(parse_custom_folders(Some("not json")).is_empty());
        assert!(parse_custom_folders(None).is_empty());
    }

    #[test]
    fn effective_folders_dedups_default() {
        let folders = effective_folders(&[]);
        assert!(!folders.is_empty());
        assert!(!folders[0].custom);
        let dup = vec![folders[0].path.clone()];
        assert_eq!(effective_folders(&dup).len(), folders.len());
    }
}
