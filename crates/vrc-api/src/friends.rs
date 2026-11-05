//! Friend list endpoints: `GET /auth/user/friends` and `GET /favorites`.
//!
//! Both endpoints are paginated (`n`/`offset`, max page size 100 per the
//! community-documented API). Every page still travels through the single
//! rate-limited [`Client::send`] path (RULES.md §3); fetching stops on the
//! first short page.

use crate::models::{Favorite, LimitedUser};
use crate::{ApiError, Client};

/// Documented maximum page size for list endpoints.
const PAGE_SIZE: usize = 100;

/// Upper bound on pages fetched per call — an endpoint that never returns a
/// short page must not loop forever (at most 5000 entries).
const MAX_PAGES: usize = 50;

impl Client {
    /// `GET /auth/user/friends?offline={offline}` — every page merged into
    /// one list.
    ///
    /// `offline = false` returns friends currently online or active;
    /// `offline = true` returns the rest. Pagination continues until a short
    /// page is returned, capped at [`MAX_PAGES`].
    pub async fn friends(&self, offline: bool) -> Result<Vec<LimitedUser>, ApiError> {
        let mut friends = Vec::new();
        let mut offset = 0usize;
        for _ in 0..MAX_PAGES {
            let page: Vec<LimitedUser> = self
                .get_json(&format!(
                    "/auth/user/friends?offline={offline}&n={PAGE_SIZE}&offset={offset}"
                ))
                .await?;
            let len = page.len();
            friends.extend(page);
            if len < PAGE_SIZE {
                return Ok(friends);
            }
            offset += len;
        }
        tracing::warn!("friends pagination hit the {MAX_PAGES}-page cap; returning partial list");
        Ok(friends)
    }

    /// `GET /favorites?type=friend` — ids of favorited friends, all pages
    /// (capped at [`MAX_PAGES`]).
    pub async fn favorite_friend_ids(&self) -> Result<Vec<String>, ApiError> {
        let mut ids = Vec::new();
        let mut offset = 0usize;
        for _ in 0..MAX_PAGES {
            let page: Vec<Favorite> = self
                .get_json(&format!(
                    "/favorites?type=friend&n={PAGE_SIZE}&offset={offset}"
                ))
                .await?;
            let len = page.len();
            ids.extend(
                page.into_iter()
                    .filter(|fav| fav.kind == "friend" && !fav.favorite_id.is_empty())
                    .map(|fav| fav.favorite_id),
            );
            if len < PAGE_SIZE {
                return Ok(ids);
            }
            offset += len;
        }
        tracing::warn!("favorites pagination hit the {MAX_PAGES}-page cap; returning partial list");
        Ok(ids)
    }
}
