//! Instance endpoints: lookup, create, self-invite.
//!
//! `GET /instances/{worldId}:{instanceId}` returns the documented
//! `Instance` object; `users` is populated only for instances owned by the
//! requesting user. Creation goes through `POST /instances` with the
//! documented `CreateInstanceRequest` body.

use reqwest::Method;
use serde::Serialize;

use crate::groups::{valid_group_id, valid_group_role_id};
use crate::models::{Instance, Notification};
use crate::users::valid_user_id;
use crate::worlds::valid_world_id;
use crate::{ApiError, Client};

/// `true` for a well-formed instance id — safe charset only
/// (alphanumerics plus `_ ~ ( ) - +`, the documented `~hidden(…)~region(…)`
/// forms), no path/query separators, no `..`.
pub fn valid_instance_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '~' | '(' | ')' | '-' | '+'))
}

/// `true` for a well-formed `wrld_…` world id.
pub fn valid_world_segment(world_id: &str) -> bool {
    valid_world_id(world_id)
}

fn check_path_ids(world_id: &str, instance_id: &str) -> Result<(), ApiError> {
    if !valid_world_segment(world_id) {
        return Err(ApiError::InvalidId(world_id.chars().take(64).collect()));
    }
    if !valid_instance_id(instance_id) {
        return Err(ApiError::InvalidId(instance_id.chars().take(64).collect()));
    }
    Ok(())
}

/// Documented `InstanceType` values for `POST /instances`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceKind {
    /// Public instance.
    Public,
    /// Hidden instance (`~hidden(…)`).
    Hidden,
    /// Friends-only instance.
    Friends,
    /// Private instance.
    Private,
    /// Group instance (`ownerId` = `grp_…`).
    Group,
}

impl InstanceKind {
    /// The documented wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Hidden => "hidden",
            Self::Friends => "friends",
            Self::Private => "private",
            Self::Group => "group",
        }
    }
}

/// Documented `InstanceRegion` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceRegion {
    /// US region.
    Us,
    /// US East region.
    Use,
    /// Europe region.
    Eu,
    /// Japan region.
    Jp,
    /// Unknown/any region.
    Unknown,
}

impl InstanceRegion {
    /// The documented wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Us => "us",
            Self::Use => "use",
            Self::Eu => "eu",
            Self::Jp => "jp",
            Self::Unknown => "unknown",
        }
    }
}

/// Documented `GroupAccessType` values for group instances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupAccessType {
    /// Public access.
    Public,
    /// VRChat Plus access.
    Plus,
    /// Members-only access.
    Members,
}

impl GroupAccessType {
    /// The documented wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Plus => "plus",
            Self::Members => "members",
        }
    }
}

/// `POST /instances` request body (`CreateInstanceRequest`). Only the
/// documented fields needed for world/group instance creation are modelled;
/// unknown response keys are ignored on decode.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateInstanceRequest {
    /// `wrld_…` world to instance.
    pub world_id: String,
    /// Instance type (`type` on the wire).
    #[serde(rename = "type")]
    pub kind: InstanceKind,
    /// Instance region.
    pub region: InstanceRegion,
    /// `grp_…` for group instances, `usr_…` for user-owned instances.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
    /// Group access type when `kind == Group`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_access_type: Option<GroupAccessType>,
    /// `grol_…` roles allowed to join a members-only group instance.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub role_ids: Vec<String>,
    /// Optional display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Optional description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Queue enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_enabled: Option<bool>,
    /// Invite+ for private instances.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_request_invite: Option<bool>,
    /// Age gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_gate: Option<bool>,
    /// ISO-8601 close time (ignored for public instances).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
}

impl CreateInstanceRequest {
    /// Minimal request for a world instance.
    pub fn new(world_id: impl Into<String>, kind: InstanceKind, region: InstanceRegion) -> Self {
        Self {
            world_id: world_id.into(),
            kind,
            region,
            owner_id: None,
            group_access_type: None,
            role_ids: Vec::new(),
            display_name: None,
            description: None,
            queue_enabled: None,
            can_request_invite: None,
            age_gate: None,
            closed_at: None,
        }
    }

    /// Validate every id that can become a path/body segment before send.
    fn validate(&self) -> Result<(), ApiError> {
        if !valid_world_id(&self.world_id) {
            return Err(ApiError::InvalidId(
                self.world_id.chars().take(64).collect(),
            ));
        }
        if let Some(owner) = &self.owner_id {
            let ok = if self.kind == InstanceKind::Group {
                valid_group_id(owner)
            } else {
                valid_user_id(owner)
            };
            if !ok {
                return Err(ApiError::InvalidId(owner.chars().take(64).collect()));
            }
        }
        if self.kind == InstanceKind::Group && self.owner_id.is_none() {
            return Err(ApiError::InvalidId(
                "group instance requires ownerId".to_string(),
            ));
        }
        if self.kind != InstanceKind::Group && self.group_access_type.is_some() {
            return Err(ApiError::InvalidId(
                "groupAccessType requires a group instance".to_string(),
            ));
        }
        for role in &self.role_ids {
            if !valid_group_role_id(role) {
                return Err(ApiError::InvalidId(role.chars().take(64).collect()));
            }
        }
        Ok(())
    }
}

impl Client {
    /// `GET /instances/{worldId}:{instanceId}` — instance details, including
    /// `users` when the instance belongs to the requesting user.
    pub async fn instance(&self, world_id: &str, instance_id: &str) -> Result<Instance, ApiError> {
        check_path_ids(world_id, instance_id)?;
        self.get_json(&format!("/instances/{world_id}:{instance_id}"))
            .await
    }

    /// `POST /instances` — create an instance; returns the `Instance`.
    pub async fn create_instance(
        &self,
        request: &CreateInstanceRequest,
    ) -> Result<Instance, ApiError> {
        request.validate()?;
        self.send_json(
            Method::POST,
            "/instances",
            Some(&serde_json::to_value(request).map_err(ApiError::Decode)?),
        )
        .await
    }

    /// `POST /invite/myself/to/{worldId}:{instanceId}` — send yourself an
    /// invite to an instance; returns the invite notification.
    pub async fn invite_myself_to_instance(
        &self,
        world_id: &str,
        instance_id: &str,
    ) -> Result<Notification, ApiError> {
        check_path_ids(world_id, instance_id)?;
        self.send_json(
            Method::POST,
            &format!("/invite/myself/to/{world_id}:{instance_id}"),
            None,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_id_validation() {
        assert!(valid_instance_id(
            "12345~hidden(usr_x)~region(eu)~nonce(abc)"
        ));
        assert!(valid_instance_id("offline"));
        assert!(valid_instance_id("private"));
        assert!(!valid_instance_id(""));
        assert!(!valid_instance_id("a/b"));
        assert!(!valid_instance_id("a?b=c"));
        assert!(!valid_instance_id("a&b=c"));
        assert!(!valid_instance_id("a..b"));
        assert!(!valid_instance_id("a:b"));
    }

    #[test]
    fn create_instance_request_validates_ids() {
        let mut req = CreateInstanceRequest::new(
            "wrld_00000000-0000-0000-0000-000000000001",
            InstanceKind::Group,
            InstanceRegion::Us,
        );
        req.owner_id = Some("grp_00000000-0000-0000-0000-000000000002".to_string());
        req.group_access_type = Some(GroupAccessType::Members);
        req.validate().unwrap();

        req.owner_id = Some("usr_00000000-0000-0000-0000-000000000003".to_string());
        assert!(req.validate().is_err());

        let mut bad_world =
            CreateInstanceRequest::new("wrld_../x", InstanceKind::Hidden, InstanceRegion::Eu);
        assert!(bad_world.validate().is_err());
        bad_world.world_id = "wrld_00000000-0000-0000-0000-000000000001".to_string();
        bad_world.owner_id = Some("grp_00000000-0000-0000-0000-000000000002".to_string());
        assert!(bad_world.validate().is_err());
    }
}
