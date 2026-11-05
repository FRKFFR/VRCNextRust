//! Pure view functions over [`AppState`]: friend sections, live search and
//! the friends' worlds dashboard. No I/O; fully unit-testable.
//!
//! Sections are disjoint: a friend appears in exactly one of `favorites`,
//! `in_game`, `active` or `offline`. `favorites` holds online favorites
//! only — an offline favorite lives in `offline`.

use std::collections::BTreeMap;

use crate::state::{AppState, Friend};

/// Friends grouped for display: favorites, in-game, on the web/app, offline.
#[derive(Debug, Clone, Default)]
pub struct FriendSections {
    /// Online favorited friends (any presence other than offline).
    pub favorites: Vec<Friend>,
    /// Non-favorite friends in an instance (incl. `private`/`traveling`).
    pub in_game: Vec<Friend>,
    /// Non-favorite friends online but not in a world (web/app).
    pub active: Vec<Friend>,
    /// Friends offline (incl. favorites).
    pub offline: Vec<Friend>,
}

/// True when `location` names a real instance (`wrld_…:…`, `private`,
/// `traveling`) rather than `""`/`offline`.
fn in_game(location: &str) -> bool {
    !matches!(location, "" | "offline")
}

/// Group `state.friends` into disjoint display sections, each sorted by
/// display name (case-insensitive, then id).
///
/// `favorites`: favorites that are not offline. `in_game`: online (not
/// favorite) in a real location. `active`: not-offline, not-favorite, not
/// in a world. `offline`: the rest.
pub fn sections(state: &AppState) -> FriendSections {
    let mut out = FriendSections::default();
    for friend in state.friends.values() {
        if !friend.is_online() {
            out.offline.push(friend.clone());
        } else if friend.is_favorite {
            out.favorites.push(friend.clone());
        } else if friend.state == "online" && in_game(&friend.location) {
            out.in_game.push(friend.clone());
        } else {
            out.active.push(friend.clone());
        }
    }
    for list in [
        &mut out.favorites,
        &mut out.in_game,
        &mut out.active,
        &mut out.offline,
    ] {
        list.sort_by(|a, b| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
    }
    out
}

/// Case-insensitive live filter over `display_name` and `username`,
/// preserving the section grouping a front end renders.
pub fn search(sections: &FriendSections, query: &str) -> FriendSections {
    let needle = query.trim().to_lowercase();
    let hits = |list: &[Friend]| -> Vec<Friend> {
        list.iter()
            .filter(|f| {
                needle.is_empty()
                    || f.display_name.to_lowercase().contains(&needle)
                    || f.username.to_lowercase().contains(&needle)
            })
            .cloned()
            .collect()
    };
    FriendSections {
        favorites: hits(&sections.favorites),
        in_game: hits(&sections.in_game),
        active: hits(&sections.active),
        offline: hits(&sections.offline),
    }
}

/// One world populated by friends, for the "friends' worlds" dashboard.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorldGroup {
    /// `wrld_…` world id.
    pub world_id: String,
    /// Instance id part of the location (may be empty for bare `wrld_…`).
    pub instance_id: String,
    /// Representative full location of the group (`world:instance`).
    pub location: String,
    /// Friends currently in this world.
    pub friends: Vec<Friend>,
}

/// Group in-game friends by world. Skips `private`, `offline`, `traveling`
/// and empty locations (no world to join). Sorted by friend count
/// descending, then world id.
pub fn worlds_with_friends(state: &AppState) -> Vec<WorldGroup> {
    let mut groups: BTreeMap<String, WorldGroup> = BTreeMap::new();
    for friend in state.friends.values() {
        if friend.state != "online"
            || friend.world_id.is_empty()
            || matches!(
                friend.location.as_str(),
                "private" | "traveling" | "offline" | ""
            )
        {
            continue;
        }
        let group = groups
            .entry(friend.world_id.clone())
            .or_insert_with(|| WorldGroup {
                world_id: friend.world_id.clone(),
                instance_id: friend
                    .location
                    .split_once(':')
                    .map(|(_, inst)| inst.to_string())
                    .unwrap_or_default(),
                location: friend.location.clone(),
                friends: Vec::new(),
            });
        group.friends.push(friend.clone());
    }
    let mut out: Vec<WorldGroup> = groups.into_values().collect();
    for group in &mut out {
        group.friends.sort_by(|a, b| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
    }
    out.sort_by(|a, b| {
        b.friends
            .len()
            .cmp(&a.friends.len())
            .then_with(|| a.world_id.cmp(&b.world_id))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use vrc_api::TrustRank;

    fn friend(id: &str, name: &str, state_: &str, location: &str) -> Friend {
        Friend {
            id: id.to_string(),
            display_name: name.to_string(),
            state: state_.to_string(),
            location: location.to_string(),
            world_id: crate::state::world_id_from(location),
            trust_rank: TrustRank::KnownUser,
            ..Friend::default()
        }
    }

    fn app_state(friends: Vec<Friend>) -> AppState {
        AppState {
            friends: friends.into_iter().map(|f| (f.id.clone(), f)).collect(),
            ..AppState::default()
        }
    }

    #[test]
    fn sections_are_disjoint() {
        let mut fav_online = friend("usr_a", "Alice", "online", "wrld_x:1");
        fav_online.is_favorite = true;
        let mut fav_offline = friend("usr_b", "bob", "offline", "offline");
        fav_offline.is_favorite = true;
        let s = sections(&app_state(vec![
            fav_online,
            fav_offline,
            friend("usr_c", "Carol", "online", "private"),
            friend("usr_d", "dave", "active", ""),
            friend("usr_e", "Erin", "online", ""),
            friend("usr_f", "fay", "offline", ""),
        ]));
        assert_eq!(s.favorites.len(), 1);
        assert_eq!(s.favorites[0].id, "usr_a");
        assert_eq!(s.in_game.len(), 1);
        assert_eq!(s.in_game[0].id, "usr_c");
        // "dave" (active) and "Erin" (online, no instance) are on web/app.
        assert_eq!(s.active.len(), 2);
        // The offline favorite lands in `offline`, not `favorites`.
        assert_eq!(s.offline.len(), 2);
        assert!(s.offline.iter().any(|f| f.id == "usr_b"));
        // No friend appears twice across sections.
        let total = s.favorites.len() + s.in_game.len() + s.active.len() + s.offline.len();
        assert_eq!(total, 6);
    }

    #[test]
    fn search_filters_inside_sections_case_insensitively() {
        let mut carol = friend("usr_c", "Carol", "active", "");
        carol.username = "carol_vrc".to_string();
        let s = sections(&app_state(vec![
            friend("usr_a", "Alice", "online", "wrld_x:1"),
            carol,
        ]));
        let hits = search(&s, "CAR");
        assert_eq!(hits.active.len(), 1);
        assert_eq!(hits.active[0].id, "usr_c");
        assert!(hits.in_game.is_empty());
        let hits = search(&s, "carol_vrc");
        assert_eq!(hits.active.len(), 1);
        assert_eq!(
            search(&s, "").active.len() + search(&s, "").in_game.len(),
            2
        );
        assert_eq!(search(&s, "zzz").offline.len(), 0);
    }

    #[test]
    fn worlds_skip_private_offline_traveling_and_sort_by_count() {
        let state = app_state(vec![
            friend("usr_a", "A", "online", "wrld_one:1"),
            friend("usr_b", "B", "online", "wrld_one:2"),
            friend("usr_c", "C", "online", "wrld_two:1"),
            friend("usr_d", "D", "online", "private"),
            friend("usr_e", "E", "online", "traveling"),
            friend("usr_f", "F", "online", "offline"),
            friend("usr_g", "G", "offline", "wrld_one:9"),
            friend("usr_h", "H", "active", "wrld_one:9"),
        ]);
        let groups = worlds_with_friends(&state);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].world_id, "wrld_one");
        // The representative instance is whichever friend the map visited
        // first — both belong to wrld_one.
        assert!(matches!(groups[0].instance_id.as_str(), "1" | "2"));
        assert_eq!(groups[0].friends.len(), 2);
        assert_eq!(groups[1].world_id, "wrld_two");
        assert_eq!(groups[1].friends.len(), 1);
    }
}
