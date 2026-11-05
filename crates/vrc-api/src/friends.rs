//! Friend list endpoints: `GET /friends` and `GET /favorites`.
//!
//! Both endpoints are paginated (`n`/`offset`, max page size 100 per the
//! community-documented API). Every page still travels through the single
//! rate-limited [`Client::send`] path (RULES.md §3); fetching stops on the
//! first short page.

use crate::models::{Favorite, LimitedUser};
use crate::{ApiError, Client};

/// Documented maximum page size for list endpoints.
const PAGE_SIZE: usize = 100;

impl Client {
    /// `GET /friends?offline={offline}` — every page merged into one list.
    ///
    /// `offline = false` returns friends currently online or active;
    /// `offline = true` returns the rest. Pagination continues until a short
    /// page is returned.
    pub async fn friends(&self, offline: bool) -> Result<Vec<LimitedUser>, ApiError> {
        let mut friends = Vec::new();
        let mut offset = 0usize;
        loop {
            let page: Vec<LimitedUser> = self
                .get_json(&format!(
                    "/friends?offline={offline}&n={PAGE_SIZE}&offset={offset}"
                ))
                .await?;
            let len = page.len();
            friends.extend(page);
            if len < PAGE_SIZE {
                return Ok(friends);
            }
            offset += len;
        }
    }

    /// `GET /favorites?type=friend` — ids of favorited friends, all pages.
    pub async fn favorite_friend_ids(&self) -> Result<Vec<String>, ApiError> {
        let mut ids = Vec::new();
        let mut offset = 0usize;
        loop {
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
    }
}
