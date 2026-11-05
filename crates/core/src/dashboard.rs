//! Friends' worlds dashboard: wraps [`worlds_with_friends`] with resolved
//! world display names from [`AppState::world_names`]. Pure view functions;
//! no I/O.

use crate::sections::worlds_with_friends;
use crate::state::{AppState, Friend, WorldName};

/// One card on the friends' worlds dashboard.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DashboardEntry {
    /// `wrld_…` world id.
    pub world_id: String,
    /// Resolved world display name; `None` while resolution is pending or
    /// after a failure (front ends fall back to `world_id`).
    pub world_name: Option<String>,
    /// Instance id part of the location (may be empty for bare `wrld_…`).
    pub instance_id: String,
    /// Representative full location of the group (`world:instance`).
    pub location: String,
    /// Friends currently in this world/instance.
    pub friends: Vec<Friend>,
}

/// Build the dashboard: every world populated by friends, enriched with
/// the cached world name. Sorted by friend count descending (inherited
/// from [`worlds_with_friends`]).
pub fn dashboard(state: &AppState) -> Vec<DashboardEntry> {
    worlds_with_friends(state)
        .into_iter()
        .map(|group| {
            let world_name = match state.world_names.get(&group.world_id) {
                Some(WorldName::Known(name)) if !name.is_empty() => Some(name.clone()),
                _ => None,
            };
            DashboardEntry {
                world_id: group.world_id,
                world_name,
                instance_id: group.instance_id,
                location: group.location,
                friends: group.friends,
            }
        })
        .collect()
}

/// Display title for a dashboard card: the resolved world name, or the
/// world id while the name is still loading or unknown.
pub fn world_title(entry: &DashboardEntry) -> &str {
    entry.world_name.as_deref().unwrap_or(&entry.world_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Friend;
    use std::time::Instant;
    use vrc_api::TrustRank;

    fn friend(id: &str, name: &str, location: &str) -> Friend {
        Friend {
            id: id.to_string(),
            display_name: name.to_string(),
            state: "online".to_string(),
            location: location.to_string(),
            world_id: crate::state::world_id_from(location),
            trust_rank: TrustRank::KnownUser,
            ..Friend::default()
        }
    }

    fn state_with(friends: Vec<Friend>) -> AppState {
        AppState {
            friends: friends.into_iter().map(|f| (f.id.clone(), f)).collect(),
            ..AppState::default()
        }
    }

    #[test]
    fn dashboard_wraps_worlds_and_adds_names() {
        let mut state = state_with(vec![
            friend("usr_a", "A", "wrld_one:1"),
            friend("usr_b", "B", "wrld_one:1"),
            friend("usr_c", "C", "wrld_two:9"),
        ]);
        state.world_names.insert(
            "wrld_one".to_string(),
            WorldName::Known("Test World One".into()),
        );
        state
            .world_names
            .insert("wrld_two".to_string(), WorldName::Pending);

        let entries = dashboard(&state);
        assert_eq!(entries.len(), 2);
        // Sorted by friend count: wrld_one (2 friends) first.
        assert_eq!(entries[0].world_id, "wrld_one");
        assert_eq!(entries[0].world_name.as_deref(), Some("Test World One"));
        assert_eq!(entries[0].instance_id, "1");
        assert_eq!(world_title(&entries[0]), "Test World One");
        assert_eq!(entries[0].friends.len(), 2);

        assert_eq!(entries[1].world_id, "wrld_two");
        // While pending the world id is the display title.
        assert_eq!(entries[1].world_name, None);
        assert_eq!(world_title(&entries[1]), "wrld_two");
        assert_eq!(entries[1].friends.len(), 1);
    }

    #[test]
    fn unresolved_and_failed_worlds_fall_back_to_world_id() {
        let mut state = state_with(vec![
            friend("usr_a", "A", "wrld_zzz:1"),
            friend("usr_b", "B", "wrld_yyy:2"),
        ]);
        state.world_names.insert(
            "wrld_yyy".to_string(),
            WorldName::Unknown {
                retry_at: Instant::now() + std::time::Duration::from_secs(60),
            },
        );
        let entries = dashboard(&state);
        assert_eq!(entries.len(), 2);
        for entry in &entries {
            assert_eq!(entry.world_name, None);
            assert_eq!(world_title(entry), entry.world_id.as_str());
        }
    }

    #[test]
    fn empty_state_yields_empty_dashboard() {
        assert!(dashboard(&AppState::default()).is_empty());
    }
}
