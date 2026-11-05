//! Friends' worlds dashboard: wraps [`worlds_with_friends`] with resolved
//! world display names from [`AppState::world_names`]. Pure view functions;
//! no I/O.

use crate::sections::{worlds_with_friends, WorldGroup};
use crate::state::{AppState, Friend, WorldName};

/// One card on the friends' worlds dashboard.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DashboardEntry {
    /// The underlying world/instance group (friends, location, ids).
    pub group: WorldGroup,
    /// Resolved display name, while a resolution is pending, or unknown.
    pub world_name: WorldName,
}

/// Build the dashboard: every world populated by friends, enriched with
/// the cached world name. Sorted by friend count descending (inherited
/// from [`worlds_with_friends`]).
pub fn dashboard(state: &AppState) -> Vec<DashboardEntry> {
    worlds_with_friends(state)
        .into_iter()
        .map(|group| DashboardEntry {
            world_name: state
                .world_names
                .get(&group.world_id)
                .cloned()
                .unwrap_or_default(),
            group,
        })
        .collect()
}

/// Human-facing world name for a dashboard entry: the resolved name, an
/// ellipsis while pending, or the world id when unknown.
pub fn world_name_label(entry: &DashboardEntry) -> &str {
    match &entry.world_name {
        WorldName::Known(name) => name,
        WorldName::Pending => "…",
        WorldName::Unknown { .. } => &entry.group.world_id,
    }
}

/// Names of the friends in a dashboard group, for compact card rendering.
pub fn friend_names(entry: &DashboardEntry) -> Vec<&str> {
    entry
        .group
        .friends
        .iter()
        .map(|f: &Friend| f.display_name.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Friend;
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
        assert_eq!(entries[0].group.world_id, "wrld_one");
        assert_eq!(
            entries[0].world_name,
            WorldName::Known("Test World One".into())
        );
        assert_eq!(world_name_label(&entries[0]), "Test World One");

        assert_eq!(entries[1].group.world_id, "wrld_two");
        assert_eq!(entries[1].world_name, WorldName::Pending);
        assert_eq!(world_name_label(&entries[1]), "…");
        assert_eq!(friend_names(&entries[1]), vec!["C"]);
    }

    #[test]
    fn unresolved_world_defaults_to_unknown_with_world_id_label() {
        let state = state_with(vec![friend("usr_a", "A", "wrld_zzz:1")]);
        let entries = dashboard(&state);
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].world_name, WorldName::Unknown { .. }));
        assert_eq!(world_name_label(&entries[0]), "wrld_zzz");
    }

    #[test]
    fn empty_state_yields_empty_dashboard() {
        assert!(dashboard(&AppState::default()).is_empty());
    }
}
