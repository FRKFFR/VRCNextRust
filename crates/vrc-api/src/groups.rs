//! Group endpoints: search, detail, membership, posts, galleries, instances.
//!
//! All path ids are validated before use; search text is query-escaped by
//! the shared helpers. Every call travels through [`Client::send`].

use reqwest::Method;

use crate::models::{Group, GroupGalleryImage, GroupInstance, GroupMember, GroupPostPage};
use crate::query::{append_bool, append_page, append_query_opt};
use crate::{ApiError, Client};

fn check_prefixed_id(prefix: &str, id: &str) -> bool {
    id.strip_prefix(prefix).is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    })
}

/// `true` for a well-formed `grp_…` group id.
pub fn valid_group_id(id: &str) -> bool {
    check_prefixed_id("grp_", id)
}

/// `true` for a well-formed `ggal_…` group-gallery id.
pub fn valid_group_gallery_id(id: &str) -> bool {
    check_prefixed_id("ggal_", id)
}

/// `true` for a well-formed `grol_…` group-role id.
pub fn valid_group_role_id(id: &str) -> bool {
    check_prefixed_id("grol_", id)
}

/// `true` for a well-formed `gmem_…` group-member id.
pub fn valid_group_member_id(id: &str) -> bool {
    check_prefixed_id("gmem_", id)
}

fn check_group_id(id: &str) -> Result<(), ApiError> {
    if valid_group_id(id) {
        Ok(())
    } else {
        Err(ApiError::InvalidId(id.chars().take(64).collect()))
    }
}

impl Client {
    /// `GET /groups?query=…` — search groups.
    pub async fn search_groups(
        &self,
        query: Option<&str>,
        n: Option<u32>,
        offset: Option<u64>,
    ) -> Result<Vec<Group>, ApiError> {
        let mut path = String::from("/groups");
        append_query_opt(&mut path, "query", query);
        append_page(&mut path, n, offset);
        self.get_json(&path).await
    }

    /// `GET /groups/{groupId}` — group detail; `include_roles` adds roles.
    pub async fn group(&self, group_id: &str, include_roles: bool) -> Result<Group, ApiError> {
        check_group_id(group_id)?;
        let mut path = format!("/groups/{group_id}");
        append_bool(&mut path, "includeRoles", Some(include_roles));
        self.get_json(&path).await
    }

    /// `POST /groups/{groupId}/join` — join a group (returns the member).
    pub async fn join_group(
        &self,
        group_id: &str,
        confirm_override_block: bool,
    ) -> Result<GroupMember, ApiError> {
        check_group_id(group_id)?;
        let mut path = format!("/groups/{group_id}/join");
        append_bool(
            &mut path,
            "confirmOverrideBlock",
            Some(confirm_override_block),
        );
        self.send_json(Method::POST, &path, Some(&serde_json::json!({})))
            .await
    }

    /// `POST /groups/{groupId}/leave` — leave a group (empty 200).
    pub async fn leave_group(&self, group_id: &str) -> Result<(), ApiError> {
        check_group_id(group_id)?;
        self.send_empty(Method::POST, &format!("/groups/{group_id}/leave"))
            .await
    }

    /// `GET /groups/{groupId}/members` — one page of members.
    pub async fn group_members(
        &self,
        group_id: &str,
        n: Option<u32>,
        offset: Option<u64>,
        sort: Option<&str>,
        role_id: Option<&str>,
    ) -> Result<Vec<GroupMember>, ApiError> {
        check_group_id(group_id)?;
        if let Some(role) = role_id {
            if !valid_group_role_id(role) {
                return Err(ApiError::InvalidId(role.chars().take(64).collect()));
            }
        }
        let mut path = format!("/groups/{group_id}/members");
        append_page(&mut path, n, offset);
        append_query_opt(&mut path, "sort", sort);
        append_query_opt(&mut path, "roleId", role_id);
        self.get_json(&path).await
    }

    /// `GET /groups/{groupId}/members/search` — search members (query ≥3).
    pub async fn search_group_members(
        &self,
        group_id: &str,
        query: &str,
        n: Option<u32>,
        offset: Option<u64>,
    ) -> Result<Vec<GroupMember>, ApiError> {
        check_group_id(group_id)?;
        let query = query.trim();
        if query.chars().count() < 3 {
            return Err(ApiError::InvalidId("member search query".to_string()));
        }
        let mut path = format!("/groups/{group_id}/members/search");
        append_page(&mut path, n, offset);
        append_query_opt(&mut path, "query", Some(query));
        self.get_json(&path).await
    }

    /// `GET /groups/{groupId}/members/{userId}` — one member.
    pub async fn group_member(
        &self,
        group_id: &str,
        user_id: &str,
    ) -> Result<GroupMember, ApiError> {
        check_group_id(group_id)?;
        if !crate::valid_user_id(user_id) {
            return Err(ApiError::InvalidId(user_id.chars().take(64).collect()));
        }
        self.get_json(&format!("/groups/{group_id}/members/{user_id}"))
            .await
    }

    /// `GET /groups/{groupId}/posts` — `{posts,total}` envelope.
    pub async fn group_posts(
        &self,
        group_id: &str,
        n: Option<u32>,
        offset: Option<u64>,
        public_only: bool,
    ) -> Result<GroupPostPage, ApiError> {
        check_group_id(group_id)?;
        let mut path = format!("/groups/{group_id}/posts");
        append_page(&mut path, n, offset);
        append_bool(&mut path, "publicOnly", Some(public_only));
        self.get_json(&path).await
    }

    /// `GET /groups/{groupId}/galleries/{galleryId}` — gallery images.
    pub async fn group_gallery_images(
        &self,
        group_id: &str,
        gallery_id: &str,
        n: Option<u32>,
        offset: Option<u64>,
        approved: Option<bool>,
    ) -> Result<Vec<GroupGalleryImage>, ApiError> {
        check_group_id(group_id)?;
        if !valid_group_gallery_id(gallery_id) {
            return Err(ApiError::InvalidId(gallery_id.chars().take(64).collect()));
        }
        let mut path = format!("/groups/{group_id}/galleries/{gallery_id}");
        append_page(&mut path, n, offset);
        append_bool(&mut path, "approved", approved);
        self.get_json(&path).await
    }

    /// `GET /groups/{groupId}/instances` — active group instances.
    pub async fn group_instances(&self, group_id: &str) -> Result<Vec<GroupInstance>, ApiError> {
        check_group_id(group_id)?;
        self.get_json(&format!("/groups/{group_id}/instances"))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_id_validation() {
        assert!(valid_group_id("grp_00000000-0000-0000-0000-000000000001"));
        assert!(valid_group_gallery_id(
            "ggal_00000000-0000-0000-0000-000000000001"
        ));
        assert!(valid_group_role_id(
            "grol_00000000-0000-0000-0000-000000000001"
        ));
        assert!(valid_group_member_id(
            "gmem_00000000-0000-0000-0000-000000000001"
        ));
        for bad in ["grp_", "usr_123", "grp_../x", "grp_a?b=c", "grp_a b", ""] {
            assert!(!valid_group_id(bad), "{bad}");
        }
        assert!(!valid_group_gallery_id("grp_123"));
        assert!(!valid_group_role_id("ggal_123"));
        assert!(!valid_group_member_id("gmem_a/b"));
    }
}
