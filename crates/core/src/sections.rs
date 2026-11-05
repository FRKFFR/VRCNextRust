//! Pure view functions over [`AppState`]: friend sections, live search and
//! the friends' worlds dashboard. No I/O; fully unit-testable.

use std::collections::BTreeMap;

use crate::state::{AppState, Friend};

/// Friends grouped for display: favorites, in-game, on the web/app, offline.
///
/// `favorites` overlaps the presence sections — a favorited online friend
/// appears twice (mirroring how the in-game list works).
#[derive(Debug, Clone, Default)]
pub struct FriendSections {
    /// Favorited friends.
    pub favorites: Vec<Friend>,
    /// Friends in an instance (incl. `private`/`traveling` locations).
    pub in_game: Vec<Friend>,
    /// Friends online but not in a world (web/app).
    pub active: Vec<Friend>,
    /// Friends offline.
    pub offline: Vec<Friend>,
}

/// True when `location` names a real instance (`wrld_…:…`, `private`,
/// `traveling`) rather than `""`/`offline`.
fn in_game(location: &str) -> bool {
    !matches!(location, "" | "offline")
}

/// Group `state.friends` into display sections, each sorted by display name
/// (case-insensitive, then id).
pub fn sections(state: &AppState) -> FriendSections {
    let mut out = FriendSections::default();
    for friend in state.friends.values() {
        if friend.is_favorite {
            out.favorites.push(friend.clone());
        }
        if !friend.is_online {
            out.offline.push(friend.clone());
        } else if in_game(&friend.location) {
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
/// sorted like the sections.
pub fn search<'a>(state: &'a AppState, query: &str) -> Vec<&'a Friend> {
    let needle = query.trim().to_lowercase();
    let mut hits: Vec<&Friend> = state
        .friends
        .values()
        .filter(|f| {
            needle.is_empty()
                || f.display_name.to_lowercase().contains(&needle)
                || f.username.to_lowercase().contains(&needle)
        })
        .collect();
    hits.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    hits
}

/// One world populated by friends, for the "friends' worlds" dashboard.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorldGroup {
    /// `wrld_…` world id.
    pub world_id: String,
    /// Representative instance location of the group.
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
        if !friend.is_online
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

    fn friend(id: &str, name: &str, online: bool, location: &str) -> Friend {
        Friend {
            id: id.to_string(),
            display_name: name.to_string(),
            is_online: online,
            location: location.to_string(),
            world_id: crate::state::world_id_from(location),
            trust_rank: TrustRank::KnownUser,
            ..Friend::default()
        }
    }

    fn state(friends: Vec<Friend>) -> AppState {
        AppState {
            friends: friends.into_iter().map(|f| (f.id.clone(), f)).collect(),
            ..AppState::default()
        }
    }

    #[test]
    fn sections_split_favorites_in_game_active_offline() {
        let mut fav = friend("usr_a", "Alice", true, "wrld_x:1");
        fav.is_favorite = true;
        let state = state(vec![
            fav,
            friend("usr_b", "bob", true, "private"),
            friend("usr_c", "Carol", true, ""),
            friend("usr_d", "dave", false, "offline"),
        ]);
        let s = sections(&state);
        assert_eq!(s.favorites.len(), 1);
        assert_eq!(s.in_game.len(), 2);
        assert_eq!(s.active.len(), 1);
        assert_eq!(s.offline.len(), 1);
        // Display names sort case-insensitively.
        assert_eq!(
            s.in_game[0].display_name.to_lowercase(),
            "alice".to_string()
        );
    }

    #[test]
    fn search_is_case_insensitive_on_name_and_username() {
        let mut carol = friend("usr_c", "Carol", true, "");
        carol.username = "carol_vrc".to_string();
        let state = state(vec![friend("usr_a", "Alice", true, ""), carol]);
        assert_eq!(search(&state, "CAR").len(), 1);
        assert_eq!(search(&state, "carol_vrc")[0].id, "usr_c");
        assert_eq!(search(&state, "").len(), 2);
        assert_eq!(search(&state, "zzz").len(), 0);
    }

    #[test]
    fn worlds_skip_private_offline_traveling_and_sort_by_count() {
        let state = state(vec![
            friend("usr_a", "A", true, "wrld_one:1"),
            friend("usr_b", "B", true, "wrld_one:2"),
            friend("usr_c", "C", true, "wrld_two:1"),
            friend("usr_d", "D", true, "private"),
            friend("usr_e", "E", true, "traveling"),
            friend("usr_f", "F", true, "offline"),
            friend("usr_g", "G", false, "wrld_one:9"),
        ]);
        let groups = worlds_with_friends(&state);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].world_id, "wrld_one");
        assert_eq!(groups[0].friends.len(), 2);
        assert_eq!(groups[1].world_id, "wrld_two");
        assert_eq!(groups[1].friends.len(), 1);
    }
}
