//! M5 browse/search helpers: debounce constants, query normalization and
//! pure view helpers. No I/O, no UI deps (RULES.md §7).

use std::time::Duration;

use vrc_api::FavoriteKind;

use crate::state::{AppState, BrowsePage};

/// Debounce applied to REST search commands before the request is sent.
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(400);

/// How long a failed detail fetch is cached before a retry is allowed.
pub const DETAIL_RETRY: Duration = Duration::from_secs(60);

/// Page size used for M5 browse/search/favorite lists.
pub const BROWSE_PAGE_SIZE: usize = 24;

/// Which top-level browse feed a debounced command belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchKind {
    /// `GET /worlds`.
    Worlds,
    /// `GET /worlds/active`.
    ActiveWorlds,
    /// `GET /worlds/recent`.
    RecentWorlds,
    /// `GET /groups`.
    Groups,
    /// `GET /avatars` (featured vs. own is part of the page key).
    Avatars,
}

/// Normalize a user search query: trim, collapse nothing else (the API
/// does its own matching). Empty input stays empty — callers decide whether
/// an empty query means "browse defaults" or "no search".
pub fn normalize_query(query: &str) -> String {
    query.trim().to_string()
}

/// `Some(query)` when non-empty after normalization.
pub fn non_empty_query(query: &str) -> Option<String> {
    let normalized = normalize_query(query);
    (!normalized.is_empty()).then_some(normalized)
}

/// Key used for `group_gallery_images` pages (`group_id:gallery_id`).
pub fn gallery_key(group_id: &str, gallery_id: &str) -> String {
    format!("{group_id}:{gallery_id}")
}

/// Split a [`gallery_key`] back into `(group_id, gallery_id)`.
pub fn parse_gallery_key(key: &str) -> Option<(&str, &str)> {
    key.split_once(':')
}

/// Find the `/favorites` record for `object_id` of `kind` (used to turn a
/// world/avatar id into the `fvrt_…` id `DELETE /favorites/{id}` needs).
pub fn favorite_record<'a>(
    state: &'a AppState,
    kind: FavoriteKind,
    object_id: &str,
) -> Option<&'a vrc_api::Favorite> {
    state
        .favorite_records
        .iter()
        .find(|fav| fav.kind == kind.as_str() && fav.favorite_id == object_id)
}

/// Whether `object_id` of `kind` is currently favorited (record loaded).
pub fn is_favorite(state: &AppState, kind: FavoriteKind, object_id: &str) -> bool {
    favorite_record(state, kind, object_id).is_some()
}

/// Display label for a favorite kind (CLI/UI plain text).
pub fn favorite_kind_label(kind: FavoriteKind) -> &'static str {
    match kind {
        FavoriteKind::World => "world",
        FavoriteKind::Avatar => "avatar",
        FavoriteKind::Friend => "friend",
    }
}

/// A page's items plus a "more pages may exist" hint for pagers.
pub fn page_has_more<T>(page: &BrowsePage<T>) -> bool {
    page.has_more(BROWSE_PAGE_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_and_non_empty_query() {
        assert_eq!(normalize_query("  test  "), "test");
        assert_eq!(non_empty_query("   "), None);
        assert_eq!(non_empty_query(" x ").as_deref(), Some("x"));
    }

    #[test]
    fn gallery_key_roundtrip() {
        let key = gallery_key("grp_a", "ggal_b");
        assert_eq!(parse_gallery_key(&key), Some(("grp_a", "ggal_b")));
        assert_eq!(parse_gallery_key("nogroup"), None);
    }
}
