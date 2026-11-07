//! M6 thumbnail pipeline: decode images off the UI thread into bounded
//! RGBA payloads cached in a session LRU.
//!
//! The cache lives in `vrc-core` (no egui types — RULES.md §7); the UI
//! turns each [`ThumbnailImage`] into a texture on its own side and
//! keeps its own `TextureHandle` map. Local files and remote image bytes
//! share the same decode path so dimension/alloc limits apply uniformly.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Long-edge cap for generated thumbnails (square-ish grid cells).
pub const THUMB_MAX_DIM: u32 = 256;

/// Entries kept in the LRU; each thumbnail is at most
/// `THUMB_MAX_DIM² × 4` bytes ≈ 256 KiB, so the cache stays ≤ ~128 MiB.
const CACHE_CAPACITY: usize = 512;

/// Refuse to decode images larger than this on either axis.
const MAX_DECODE_DIM: u32 = 16_384;

/// Decoder allocation cap (the `image` default is 512 MiB; thumbnails
/// never need that much).
const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;

/// Local photo files larger than this are not thumbnailed.
pub const MAX_LOCAL_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Remote image fetch cap (bytes) enforced by `Client::get_bytes`.
pub const MAX_REMOTE_IMAGE_BYTES: usize = 32 * 1024 * 1024;

/// Session-scoped negative cache cap (failed keys never retried).
const FAILURE_CAP: usize = 1024;

/// A decoded thumbnail: raw RGBA8 pixels + dimensions. UI-neutral — the
/// front end uploads `rgba` to the GPU itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailImage {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// `width * height * 4` RGBA bytes.
    pub rgba: Vec<u8>,
}

/// Where a thumbnail's pixels come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThumbSource {
    /// A local photo file (absolute path).
    File(String),
    /// A remote image URL (already allowlist-validated by the caller).
    Url(String),
}

impl ThumbSource {
    /// Cache key for this source (paths and URLs never collide in
    /// practice; the namespaces are disjoint strings).
    pub fn key(&self) -> String {
        match self {
            ThumbSource::File(path) => format!("file:{path}"),
            ThumbSource::Url(url) => format!("url:{url}"),
        }
    }
}

struct CacheEntry {
    image: Arc<ThumbnailImage>,
    /// LRU tick — higher is more recently used.
    tick: u64,
}

#[derive(Default)]
struct Inner {
    map: HashMap<String, CacheEntry>,
    /// Keys with a decode/fetch in flight (in-flight dedup).
    in_flight: HashSet<String>,
    /// Keys that failed — never retried this session.
    failed: HashSet<String>,
    tick: AtomicU64,
}

/// Cheaply cloneable handle to the shared thumbnail cache.
#[derive(Clone, Default)]
pub struct ThumbnailHandle {
    inner: Arc<Mutex<Inner>>,
}

impl ThumbnailHandle {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fetch a decoded thumbnail, marking it most-recently-used.
    pub fn get(&self, key: &str) -> Option<Arc<ThumbnailImage>> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let tick = inner.tick.fetch_add(1, Ordering::Relaxed);
        let entry = inner.map.get_mut(key)?;
        entry.tick = tick;
        Some(entry.image.clone())
    }

    /// Store a decoded thumbnail, evicting the least-recently-used entry
    /// when at capacity.
    pub fn insert(&self, key: String, image: ThumbnailImage) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let tick = inner.tick.fetch_add(1, Ordering::Relaxed);
        if inner.map.len() >= CACHE_CAPACITY && !inner.map.contains_key(&key) {
            if let Some(oldest) = inner
                .map
                .iter()
                .min_by_key(|(_, e)| e.tick)
                .map(|(k, _)| k.clone())
            {
                inner.map.remove(&oldest);
            }
        }
        inner.in_flight.remove(&key);
        inner.map.insert(
            key,
            CacheEntry {
                image: Arc::new(image),
                tick,
            },
        );
    }

    /// Record a decode/fetch failure: the key is negatively cached so UI
    /// polling doesn't re-request it every frame.
    pub fn fail(&self, key: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.in_flight.remove(key);
        if inner.failed.len() >= FAILURE_CAP {
            inner.failed.clear();
        }
        inner.failed.insert(key.to_string());
    }

    /// Claim `key` for decoding. Returns `false` when the image is
    /// already cached, in flight, or known-bad.
    pub fn begin(&self, key: &str) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.map.contains_key(key)
            || inner.in_flight.contains(key)
            || inner.failed.contains(key)
        {
            return false;
        }
        inner.in_flight.insert(key.to_string());
        true
    }

    /// Release an in-flight claim without caching anything (used when a
    /// request is dropped, e.g. logout).
    #[allow(dead_code)]
    pub fn cancel(&self, key: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.in_flight.remove(key);
    }
}

/// Decode `bytes` (any `image`-supported container the enabled features
/// cover — png/jpeg here) into a bounded RGBA thumbnail.
pub fn decode_bytes(bytes: &[u8]) -> Option<ThumbnailImage> {
    decode_from(Cursor::new(bytes))
}

/// Decode a local image file into a bounded RGBA thumbnail. Files over
/// [`MAX_LOCAL_FILE_BYTES`] are refused up front.
pub fn decode_file(path: &Path) -> Option<ThumbnailImage> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_LOCAL_FILE_BYTES {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    decode_from(std::io::BufReader::new(file))
}

fn decode_from<R>(reader: R) -> Option<ThumbnailImage>
where
    R: std::io::BufRead + std::io::Seek,
{
    let mut reader = image::ImageReader::new(reader).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_DIM);
    limits.max_image_height = Some(MAX_DECODE_DIM);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    // `thumbnail` upscales small sources — keep already-small images as-is.
    let rgba = if image.width() <= THUMB_MAX_DIM && image.height() <= THUMB_MAX_DIM {
        image.to_rgba8()
    } else {
        image.thumbnail(THUMB_MAX_DIM, THUMB_MAX_DIM).to_rgba8()
    };
    let (width, height) = (rgba.width(), rgba.height());
    Some(ThumbnailImage {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid 1×1 PNG encoded by the `image` crate itself.
    fn png_1px() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]));
        let mut buf = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }

    #[test]
    fn decode_png_bytes() {
        let image = decode_bytes(&png_1px()).expect("1px png decodes");
        assert_eq!(image.width, 1);
        assert_eq!(image.height, 1);
        assert_eq!(image.rgba.len(), 4);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode_bytes(b"definitely not an image").is_none());
        assert!(decode_bytes(&[]).is_none());
    }

    #[test]
    fn cache_lru_and_dedup() {
        let cache = ThumbnailHandle::new();
        let key = "k".to_string();
        assert!(cache.begin(&key));
        assert!(!cache.begin(&key)); // in-flight dedup
        cache.insert(
            key.clone(),
            ThumbnailImage {
                width: 1,
                height: 1,
                rgba: vec![0; 4],
            },
        );
        assert!(!cache.begin(&key)); // cached
        assert!(cache.get(&key).is_some());
        cache.fail("bad");
        assert!(!cache.begin("bad")); // negative cache
    }

    #[test]
    fn source_keys_are_namespaced() {
        assert_ne!(
            ThumbSource::File("x".into()).key(),
            ThumbSource::Url("x".into()).key()
        );
    }
}
