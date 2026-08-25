//! Nested folders for organising profiles.
//!
//! Folders are purely organisational — nothing about syncing depends on them.
//! A folder's parent is another folder, or `None` for the top level, and a
//! profile's `folder_id` works the same way.
//!
//! Folders and profiles share one ordering per level, so a folder can sit
//! between two profiles. Two separate orderings could not express that, and the
//! list draws a single sequence — a drop between any two rows has to mean what
//! it looks like.
//!
//! Two things can corrupt a tree, and both are handled here rather than in the
//! UI so the stored data is always renderable: a folder made its own ancestor
//! would detach an entire branch from the root, and a reference to a folder
//! that no longer exists would hide whatever pointed at it.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::store::Config;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// Containing folder, or `None` at the top level.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// Place within `parent_id`, in an order shared with the profiles that sit
    /// at the same level. See [`children`].
    #[serde(default)]
    pub position: u32,
}

impl Folder {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Folder name is required.".into());
        }
        Ok(())
    }
}

/// Whether `candidate` is `ancestor` or sits somewhere beneath it.
///
/// Walks upwards from `candidate`, and gives up after as many steps as there
/// are folders so an already-cyclic tree cannot spin here forever.
pub fn is_self_or_descendant(folders: &[Folder], candidate: &str, ancestor: &str) -> bool {
    let mut current = Some(candidate.to_string());
    for _ in 0..=folders.len() {
        let Some(id) = current else { return false };
        if id == ancestor {
            return true;
        }
        current = folders
            .iter()
            .find(|f| f.id == id)
            .and_then(|f| f.parent_id.clone());
    }
    false
}

/// Reject a move that would detach a branch from the root.
pub fn check_move(folders: &[Folder], folder_id: &str, new_parent: Option<&str>) -> Result<(), String> {
    let Some(parent) = new_parent else { return Ok(()) };
    if is_self_or_descendant(folders, parent, folder_id) {
        return Err("A folder cannot be moved inside itself.".into());
    }
    Ok(())
}

/// What a deletion took with it.
pub struct Removed {
    /// Ids of the profiles removed, so their watchers can be stopped. Config
    /// records alone are not enough — a deleted profile left running would keep
    /// syncing with nothing on screen to show it.
    pub profile_ids: Vec<String>,
    /// Nested folders removed, not counting the one that was asked for.
    pub folders: usize,
}

/// Remove a folder and everything inside it, however deeply nested.
///
/// Only records are deleted. Files on this Mac and on the remote host are never
/// touched, and nothing is un-synced — whatever was already sent stays where it
/// is on the remote.
pub fn remove(config: &mut Config, folder_id: &str) -> Removed {
    // The whole subtree, collected through a set rather than a plain walk so a
    // tree that is somehow cyclic cannot loop here forever.
    let mut doomed: HashSet<String> = HashSet::new();
    let mut queue = vec![folder_id.to_string()];
    while let Some(id) = queue.pop() {
        if !doomed.insert(id.clone()) {
            continue;
        }
        queue.extend(
            config
                .folders
                .iter()
                .filter(|f| f.parent_id.as_deref() == Some(id.as_str()))
                .map(|f| f.id.clone()),
        );
    }

    let profile_ids: Vec<String> = config
        .profiles
        .iter()
        .filter(|p| p.folder_id.as_deref().is_some_and(|f| doomed.contains(f)))
        .map(|p| p.id.clone())
        .collect();

    let existed = config.folders.iter().filter(|f| doomed.contains(&f.id)).count();
    config.folders.retain(|f| !doomed.contains(&f.id));
    config.profiles.retain(|p| !profile_ids.contains(&p.id));

    renumber(config);
    Removed {
        profile_ids,
        // The folder that was asked for does not count as something extra lost.
        folders: existed.saturating_sub(1),
    }
}

/// Which collection an item lives in. Folders and profiles share one ordering
/// per level, so the code that arranges a level treats them alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemKind {
    Folder,
    Profile,
}

/// Everything directly inside `parent`, in display order.
///
/// Built first in the order a config written before positions existed used to
/// render — folders by name, then profiles as stored — and then *stably* sorted
/// by position. In such a config every position is 0, so that first order is
/// what survives and nothing moves on upgrade; once anything has been dragged
/// the positions are distinct and decide it alone.
fn children(config: &Config, parent: Option<&str>) -> Vec<(ItemKind, String)> {
    let mut folders: Vec<&Folder> = config
        .folders
        .iter()
        .filter(|f| f.parent_id.as_deref() == parent)
        .collect();
    folders.sort_by(|a, b| a.name.cmp(&b.name));

    let mut items: Vec<(ItemKind, String, u32)> = folders
        .into_iter()
        .map(|f| (ItemKind::Folder, f.id.clone(), f.position))
        .chain(
            config
                .profiles
                .iter()
                .filter(|p| p.folder_id.as_deref() == parent)
                .map(|p| (ItemKind::Profile, p.id.clone(), p.position)),
        )
        .collect();

    items.sort_by_key(|&(_, _, position)| position);
    items.into_iter().map(|(kind, id, _)| (kind, id)).collect()
}

fn set_position(config: &mut Config, kind: ItemKind, id: &str, position: u32) {
    match kind {
        ItemKind::Folder => {
            if let Some(folder) = config.folders.iter_mut().find(|f| f.id == id) {
                folder.position = position;
            }
        }
        ItemKind::Profile => {
            if let Some(profile) = config.profiles.iter_mut().find(|p| p.id == id) {
                profile.position = position;
            }
        }
    }
}

fn set_parent(config: &mut Config, kind: ItemKind, id: &str, parent: Option<&str>) {
    let parent = parent.map(String::from);
    match kind {
        ItemKind::Folder => {
            if let Some(folder) = config.folders.iter_mut().find(|f| f.id == id) {
                folder.parent_id = parent;
            }
        }
        ItemKind::Profile => {
            if let Some(profile) = config.profiles.iter_mut().find(|p| p.id == id) {
                profile.folder_id = parent;
            }
        }
    }
}

fn exists(config: &Config, kind: ItemKind, id: &str) -> bool {
    match kind {
        ItemKind::Folder => config.folders.iter().any(|f| f.id == id),
        ItemKind::Profile => config.profiles.iter().any(|p| p.id == id),
    }
}

/// Give every level dense positions matching its current display order.
///
/// Run after anything that moves items between levels, so two items can never
/// share a position and the stored order is always the one on screen.
pub fn renumber(config: &mut Config) {
    let levels: Vec<Option<String>> = std::iter::once(None)
        .chain(config.folders.iter().map(|f| Some(f.id.clone())))
        .collect();

    for level in levels {
        for (index, (kind, id)) in children(config, level.as_deref())
            .into_iter()
            .enumerate()
        {
            set_position(config, kind, &id, index as u32);
        }
    }
}

/// Move an item into `parent`, placed immediately before `before` — the single
/// operation a drag ends in.
///
/// `before` names whichever item the drop landed above, of either kind, and
/// `None` means last. Folder and position travel together because a drop
/// decides both at once; splitting them would write the config twice and show
/// the item in the wrong place in between.
///
/// An item that no longer exists is a no-op rather than an error: a drag from a
/// window that has not refreshed must not be able to lose anything.
pub fn move_item(
    config: &mut Config,
    kind: ItemKind,
    id: &str,
    parent: Option<&str>,
    before: Option<&str>,
) -> Result<(), String> {
    if !exists(config, kind, id) {
        return Ok(());
    }
    if let Some(target) = parent {
        if !config.folders.iter().any(|f| f.id == target) {
            return Err("That folder no longer exists.".into());
        }
    }
    if kind == ItemKind::Folder {
        check_move(&config.folders, id, parent)?;
    }
    // Dropped on its own edge: it is already exactly there.
    if before == Some(id) {
        return Ok(());
    }

    set_parent(config, kind, id, parent);

    let mut order = children(config, parent);
    order.retain(|(_, other)| other != id);
    let at = before
        .and_then(|target| order.iter().position(|(_, other)| other == target))
        // A target that has since been deleted puts the item last rather than
        // dropping the move.
        .unwrap_or(order.len());
    order.insert(at, (kind, id.to_string()));

    for (index, (kind, id)) in order.into_iter().enumerate() {
        set_position(config, kind, &id, index as u32);
    }
    Ok(())
}

/// Force the tree into a renderable shape.
///
/// Anything pointing at a folder that no longer exists is lifted to the top
/// level, and any cycle is broken by rooting the folder that closes it. Run
/// after loading and after any change, so the UI never has to defend itself
/// against a tree it cannot draw.
pub fn normalize(config: &mut Config) {
    let ids: Vec<String> = config.folders.iter().map(|f| f.id.clone()).collect();
    let known = |id: &Option<String>| matches!(id, Some(v) if ids.iter().any(|k| k == v));

    for folder in config.folders.iter_mut() {
        if folder.parent_id.is_some() && !known(&folder.parent_id) {
            folder.parent_id = None;
        }
        // Self-parenting is the one cycle a single pass can see directly.
        if folder.parent_id.as_deref() == Some(folder.id.as_str()) {
            folder.parent_id = None;
        }
    }

    // Break longer cycles: any folder that cannot reach the root is rooted.
    let snapshot = config.folders.clone();
    for folder in config.folders.iter_mut() {
        let mut current = folder.parent_id.clone();
        let mut steps = 0;
        while let Some(id) = current {
            if steps > snapshot.len() {
                folder.parent_id = None;
                break;
            }
            current = snapshot
                .iter()
                .find(|f| f.id == id)
                .and_then(|f| f.parent_id.clone());
            steps += 1;
        }
    }

    for profile in config.profiles.iter_mut() {
        if profile.folder_id.is_some() && !known(&profile.folder_id) {
            profile.folder_id = None;
        }
    }

    // Anything lifted above changed level, so its position belongs to a level
    // it no longer sits in.
    renumber(config);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Profile;
    use crate::settings::Settings;

    fn folder(id: &str, parent: Option<&str>) -> Folder {
        Folder {
            id: id.into(),
            name: id.into(),
            parent_id: parent.map(String::from),
            position: 0,
        }
    }

    /// A level as it would be drawn: `f:` marks a folder, `p:` a profile.
    fn level(config: &Config, parent: Option<&str>) -> Vec<String> {
        children(config, parent)
            .into_iter()
            .map(|(kind, id)| match kind {
                ItemKind::Folder => format!("f:{id}"),
                ItemKind::Profile => format!("p:{id}"),
            })
            .collect()
    }

    fn profile(id: &str, folder: Option<&str>) -> Profile {
        Profile {
            id: id.into(),
            name: id.into(),
            source: "/tmp".into(),
            connection_id: "c".into(),
            remote_path: "/srv".into(),
            enabled: false,
            delete: true,
            excludes: vec![],
            debounce_ms: 500,
            folder_id: folder.map(String::from),
            position: 0,
        }
    }

    fn config(folders: Vec<Folder>, profiles: Vec<Profile>) -> Config {
        Config {
            settings: Settings::default(),
            connections: vec![],
            folders,
            profiles,
        }
    }

    #[test]
    fn folders_and_profiles_share_one_order() {
        // The whole point of the shared order: a folder can sit between two
        // profiles, which two separate orderings could not express.
        let mut cfg = config(
            vec![folder("box", None)],
            vec![profile("one", None), profile("two", None)],
        );
        renumber(&mut cfg);
        assert_eq!(level(&cfg, None), ["f:box", "p:one", "p:two"]);

        move_item(&mut cfg, ItemKind::Folder, "box", None, Some("two")).unwrap();
        assert_eq!(level(&cfg, None), ["p:one", "f:box", "p:two"]);

        // And a profile can be dropped above a folder.
        move_item(&mut cfg, ItemKind::Profile, "two", None, Some("box")).unwrap();
        assert_eq!(level(&cfg, None), ["p:one", "p:two", "f:box"]);
    }

    #[test]
    fn a_move_carries_the_folder_and_the_position_together() {
        let mut cfg = config(
            vec![folder("box", None)],
            vec![profile("one", None), profile("two", None)],
        );
        renumber(&mut cfg);

        move_item(&mut cfg, ItemKind::Profile, "two", Some("box"), None).unwrap();
        assert_eq!(cfg.profiles[1].folder_id.as_deref(), Some("box"));
        assert_eq!(level(&cfg, None), ["f:box", "p:one"]);
        assert_eq!(level(&cfg, Some("box")), ["p:two"]);

        // Dropping past the last row takes it back out.
        move_item(&mut cfg, ItemKind::Profile, "two", None, None).unwrap();
        assert_eq!(cfg.profiles[1].folder_id, None);
        assert_eq!(level(&cfg, None), ["f:box", "p:one", "p:two"]);
    }

    #[test]
    fn positions_stay_dense_and_unique_within_a_level() {
        // Two items sharing a position would make the order ambiguous, so
        // every move has to leave the level renumbered.
        let mut cfg = config(
            vec![folder("a", None), folder("b", None)],
            vec![profile("one", None), profile("two", None)],
        );
        renumber(&mut cfg);
        move_item(&mut cfg, ItemKind::Profile, "two", None, Some("a")).unwrap();

        let mut seen: Vec<u32> = cfg
            .folders
            .iter()
            .map(|f| f.position)
            .chain(cfg.profiles.iter().map(|p| p.position))
            .collect();
        seen.sort_unstable();
        assert_eq!(seen, [0, 1, 2, 3]);
    }

    #[test]
    fn a_stale_drag_can_never_lose_anything() {
        let mut cfg = config(vec![folder("box", None)], vec![profile("one", None)]);
        renumber(&mut cfg);

        // An item another window already deleted.
        move_item(&mut cfg, ItemKind::Profile, "ghost", None, None).unwrap();
        assert_eq!(level(&cfg, None), ["f:box", "p:one"]);

        // A folder another window already deleted is refused outright, rather
        // than quietly landing the profile at the top level.
        assert!(move_item(&mut cfg, ItemKind::Profile, "one", Some("ghost"), None).is_err());
        assert_eq!(cfg.profiles[0].folder_id, None);

        // A target row that is gone puts the item last instead of dropping it.
        move_item(&mut cfg, ItemKind::Profile, "one", None, Some("ghost")).unwrap();
        assert_eq!(level(&cfg, None), ["f:box", "p:one"]);
    }

    #[test]
    fn dropping_something_on_its_own_edge_changes_nothing() {
        let mut cfg = config(vec![], vec![profile("one", None), profile("two", None)]);
        renumber(&mut cfg);
        move_item(&mut cfg, ItemKind::Profile, "two", None, Some("two")).unwrap();
        assert_eq!(level(&cfg, None), ["p:one", "p:two"]);
    }

    #[test]
    fn a_config_written_before_positions_existed_keeps_its_layout() {
        // Every position is 0 there, and the app used to draw folders by name
        // first, then profiles as stored. That must survive the upgrade.
        let mut cfg = config(
            vec![folder("zulu", None), folder("alpha", None)],
            vec![profile("one", None), profile("two", None)],
        );
        assert_eq!(
            level(&cfg, None),
            ["f:alpha", "f:zulu", "p:one", "p:two"],
            "the pre-positions layout"
        );

        renumber(&mut cfg);
        assert_eq!(level(&cfg, None), ["f:alpha", "f:zulu", "p:one", "p:two"]);
    }

    #[test]
    fn a_folder_cannot_be_dragged_into_its_own_subtree() {
        let mut cfg = config(
            vec![folder("outer", None), folder("inner", Some("outer"))],
            vec![],
        );
        renumber(&mut cfg);
        assert!(move_item(&mut cfg, ItemKind::Folder, "outer", Some("inner"), None).is_err());
        assert_eq!(cfg.folders[0].parent_id, None, "left where it was");
    }

    #[test]
    fn a_folder_cannot_be_moved_into_its_own_subtree() {
        // a > b > c
        let folders = vec![folder("a", None), folder("b", Some("a")), folder("c", Some("b"))];

        assert!(check_move(&folders, "a", Some("c")).is_err(), "into a grandchild");
        assert!(check_move(&folders, "a", Some("a")).is_err(), "into itself");
        assert!(check_move(&folders, "c", Some("a")).is_ok(), "upwards is fine");
        assert!(check_move(&folders, "a", None).is_ok(), "to the top level");
    }

    #[test]
    fn deleting_a_folder_takes_its_whole_subtree_with_it() {
        // outer > inner > deep, with a profile at each level, and one outside.
        let mut cfg = config(
            vec![
                folder("outer", None),
                folder("inner", Some("outer")),
                folder("deep", Some("inner")),
                folder("elsewhere", None),
            ],
            vec![
                profile("at-outer", Some("outer")),
                profile("at-inner", Some("inner")),
                profile("at-deep", Some("deep")),
                profile("safe", Some("elsewhere")),
                profile("top", None),
            ],
        );

        let removed = remove(&mut cfg, "outer");

        assert_eq!(removed.folders, 2, "inner and deep, not counting outer");
        let mut gone = removed.profile_ids.clone();
        gone.sort();
        assert_eq!(gone, ["at-deep", "at-inner", "at-outer"]);

        // Everything outside the subtree survives untouched.
        let left: Vec<&str> = cfg.profiles.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(left, ["safe", "top"]);
        let folders: Vec<&str> = cfg.folders.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(folders, ["elsewhere"]);
    }

    #[test]
    fn the_removed_profile_ids_are_reported_so_their_watchers_can_be_stopped() {
        // Dropping the record without stopping the watcher would leave a
        // deleted profile syncing with nothing on screen to show it.
        let mut cfg = config(
            vec![folder("box", None)],
            vec![profile("inside", Some("box"))],
        );
        let removed = remove(&mut cfg, "box");
        assert_eq!(removed.profile_ids, ["inside"]);
        assert!(cfg.profiles.is_empty());
    }

    #[test]
    fn deleting_an_empty_folder_reports_nothing_else_lost() {
        let mut cfg = config(vec![folder("box", None)], vec![profile("top", None)]);
        let removed = remove(&mut cfg, "box");
        assert_eq!(removed.folders, 0);
        assert!(removed.profile_ids.is_empty());
        assert_eq!(cfg.profiles.len(), 1, "a top-level profile is not inside it");
    }

    #[test]
    fn references_to_missing_folders_surface_at_the_top_level() {
        // A profile pointing at a folder that is gone would otherwise be
        // invisible, which reads as data loss.
        let mut cfg = config(vec![], vec![profile("p", Some("ghost"))]);
        normalize(&mut cfg);
        assert_eq!(cfg.profiles[0].folder_id, None);

        let mut cfg = config(vec![folder("orphan", Some("ghost"))], vec![]);
        normalize(&mut cfg);
        assert_eq!(cfg.folders[0].parent_id, None);
    }

    #[test]
    fn a_cycle_is_broken_rather_than_hidden() {
        // a > b > a, reachable from nothing.
        let mut cfg = config(vec![folder("a", Some("b")), folder("b", Some("a"))], vec![]);
        normalize(&mut cfg);

        // Every folder can now reach the root.
        for f in &cfg.folders {
            assert!(
                !is_self_or_descendant(&cfg.folders, f.parent_id.as_deref().unwrap_or(""), &f.id),
                "{} still sits under its own subtree",
                f.id
            );
        }
    }

    #[test]
    fn self_parenting_is_undone() {
        let mut cfg = config(vec![folder("a", Some("a"))], vec![]);
        normalize(&mut cfg);
        assert_eq!(cfg.folders[0].parent_id, None);
    }

    #[test]
    fn a_healthy_tree_is_left_untouched() {
        let before = config(
            vec![folder("a", None), folder("b", Some("a"))],
            vec![profile("p", Some("b"))],
        );
        let mut after = before.clone();
        normalize(&mut after);
        assert_eq!(after.folders, before.folders);
        assert_eq!(after.profiles[0].folder_id, before.profiles[0].folder_id);
    }
}
