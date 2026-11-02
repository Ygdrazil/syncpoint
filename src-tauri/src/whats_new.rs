//! The "What's new ?" popup, shown on launch after an update or on a first run.
//!
//! The list comes from commit titles, so there is no changelog to maintain:
//! `feat:` shows as Added, `change:` as Changed and `fix:` as Fixed, and any other
//! type stays out of it. `build.rs` bakes the log into the binary.

use serde::Serialize;

use crate::settings::Settings;

const LOG: &str = include_str!(concat!(env!("OUT_DIR"), "/changelog.txt"));

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Added,
    Changed,
    Fixed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub kind: Kind,
    pub text: String,
}

/// `version` is `None` for commits newer than the latest tag, which only exist in
/// a development build.
#[derive(Debug, PartialEq, Eq)]
pub struct Release {
    version: Option<String>,
    changes: Vec<Change>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct WhatsNew {
    pub version: String,
    pub changes: Vec<Change>,
}

/// A commit title as a line in the popup, if it is the kind that gets one.
fn parse_change(title: &str) -> Option<Change> {
    let (prefix, text) = title.split_once(':')?;
    // `feat(ui)!:` is the same type as `feat:`.
    let kind = match prefix.split('(').next()?.trim_end_matches('!') {
        "feat" => Kind::Added,
        "change" => Kind::Changed,
        "fix" => Kind::Fixed,
        _ => return None,
    };
    let text = text.trim();
    let mut letters = text.chars();
    let first = letters.next()?;
    Some(Change {
        kind,
        text: first.to_uppercase().chain(letters).collect(),
    })
}

/// The three numbers of a version, ignoring what git adds after them:
/// `0.10.2-3-gab12cd3-dev` is `(0, 10, 2)`.
fn numbers(version: &str) -> Option<(u32, u32, u32)> {
    let version = version.strip_prefix('v').unwrap_or(version);
    let mut parts = version.split('-').next()?.split('.');
    let parsed = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(parsed)
}

/// The release a commit's decorations name, like `tag: v0.10.2, tag: nightly`.
fn release_tag(decorations: &str) -> Option<String> {
    decorations
        .split(", ")
        .filter_map(|decoration| decoration.strip_prefix("tag: "))
        .filter_map(|tag| Some((numbers(tag)?, tag.trim_start_matches('v'))))
        .max_by_key(|(numbers, _)| *numbers)
        .map(|(_, tag)| tag.to_string())
}

/// `git log`, newest first: a commit's tag marks where that release's commits
/// start, and everything until the next tag belongs to it.
fn parse_log(log: &str) -> Vec<Release> {
    let mut releases = vec![Release {
        version: None,
        changes: Vec::new(),
    }];
    for line in log.lines() {
        let Some((decorations, title)) = line.split_once('\t') else {
            continue;
        };
        if let Some(version) = release_tag(decorations) {
            releases.push(Release {
                version: Some(version),
                changes: Vec::new(),
            });
        }
        if let (Some(change), Some(release)) = (parse_change(title), releases.last_mut()) {
            release.changes.push(change);
        }
    }
    releases
}

/// What this launch has to show, given the version last seen.
fn pending(releases: &[Release], current: &str, last_seen: Option<&str>) -> Option<WhatsNew> {
    if last_seen == Some(current) {
        return None;
    }
    let changes: Vec<Change> = match last_seen.and_then(numbers) {
        // Everything since then, however many versions were skipped.
        Some(seen) => releases
            .iter()
            .take_while(|release| match release.version.as_deref().and_then(numbers) {
                Some(version) => version > seen,
                None => true,
            })
            .flat_map(|release| release.changes.iter().cloned())
            .collect(),
        // A first run has no history to catch up on, only the version it is on.
        None => releases
            .iter()
            .find(|release| release.version.is_some() || !release.changes.is_empty())
            .map(|release| release.changes.clone())
            .unwrap_or_default(),
    };
    (!changes.is_empty()).then(|| WhatsNew {
        version: current.to_string(),
        changes,
    })
}

/// What to show on this launch, if anything. Something shown counts as seen once
/// it is closed, so quitting without reading it shows it again. Otherwise this
/// version counts right away, so turning the popup back on later does not bring
/// a backlog of everything missed while it was off.
fn check(settings: &mut Settings, releases: &[Release], current: &str) -> Option<WhatsNew> {
    let found = if settings.show_whats_new {
        pending(releases, current, settings.last_seen_version.as_deref())
    } else {
        None
    };
    if found.is_none() {
        settings.last_seen_version = Some(current.to_string());
    }
    found
}

pub fn on_launch(settings: &mut Settings, current: &str) -> Option<WhatsNew> {
    check(settings, &parse_log(LOG), current)
}

pub fn dismiss(settings: &mut Settings, current: &str, never_again: bool) {
    settings.last_seen_version = Some(current.to_string());
    if never_again {
        settings.show_whats_new = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\tfeat: not tagged yet\n\
                       tag: v0.3.0\tfix: three fixed\n\
                       \tchore: three hidden\n\
                       \tfeat: three added\n\
                       tag: v0.2.0\tchange: two changed\n\
                       tag: v0.1.0\tfeat: one added\n";

    fn change(kind: Kind, text: &str) -> Change {
        Change {
            kind,
            text: text.into(),
        }
    }

    /// As a release build sees it, with HEAD on the tag.
    fn released() -> Vec<Release> {
        parse_log(&LOG.replacen("\tfeat: not tagged yet\n", "", 1))
    }

    fn texts(found: Option<WhatsNew>) -> Vec<String> {
        found.unwrap().changes.into_iter().map(|c| c.text).collect()
    }

    #[test]
    fn each_commit_type_gets_its_tag_and_the_rest_stay_hidden() {
        assert_eq!(parse_change("feat: x").unwrap().kind, Kind::Added);
        assert_eq!(parse_change("change: x").unwrap().kind, Kind::Changed);
        assert_eq!(parse_change("fix: x").unwrap().kind, Kind::Fixed);
        for hidden in ["chore: x", "docs: x", "refactor: x", "ci: x", "test: x", "Plain title"] {
            assert_eq!(parse_change(hidden), None, "{hidden}");
        }
    }

    #[test]
    fn a_scope_or_breaking_mark_does_not_change_the_type() {
        assert_eq!(parse_change("feat(ui): x").unwrap().kind, Kind::Added);
        assert_eq!(parse_change("fix!: x").unwrap().kind, Kind::Fixed);
        assert_eq!(parse_change("feat(ui)!: x").unwrap().kind, Kind::Added);
    }

    #[test]
    fn a_line_reads_as_a_sentence() {
        assert_eq!(parse_change("feat: add a thing").unwrap().text, "Add a thing");
        assert_eq!(parse_change("fix:   été").unwrap().text, "Été");
        assert_eq!(parse_change("feat: a: b").unwrap().text, "A: b");
        assert_eq!(parse_change("feat:   "), None, "nothing to show");
    }

    #[test]
    fn versions_compare_as_numbers_whatever_git_adds() {
        assert_eq!(numbers("0.10.2"), Some((0, 10, 2)));
        assert_eq!(numbers("v0.10.2"), Some((0, 10, 2)));
        assert_eq!(numbers("0.10.2-3-gab12cd3-dev"), Some((0, 10, 2)));
        assert!(numbers("0.10.0") > numbers("0.9.0"), "not compared as text");
        assert_eq!(numbers("ab12cd3"), None);
        assert_eq!(numbers("1.2"), None);
        assert_eq!(numbers("1.2.3.4"), None);
    }

    #[test]
    fn commits_belong_to_the_tag_at_or_above_them() {
        assert_eq!(
            parse_log(LOG),
            vec![
                Release {
                    version: None,
                    changes: vec![change(Kind::Added, "Not tagged yet")],
                },
                Release {
                    version: Some("0.3.0".into()),
                    changes: vec![
                        change(Kind::Fixed, "Three fixed"),
                        change(Kind::Added, "Three added"),
                    ],
                },
                Release {
                    version: Some("0.2.0".into()),
                    changes: vec![change(Kind::Changed, "Two changed")],
                },
                Release {
                    version: Some("0.1.0".into()),
                    changes: vec![change(Kind::Added, "One added")],
                },
            ]
        );
    }

    #[test]
    fn only_a_version_tag_starts_a_release() {
        assert_eq!(release_tag("tag: v0.2.0"), Some("0.2.0".into()));
        assert_eq!(release_tag("tag: nightly"), None);
        assert_eq!(release_tag("HEAD -> main, origin/main"), None);
        assert_eq!(release_tag("tag: v0.2.0, tag: v0.10.0, tag: nightly"), Some("0.10.0".into()));
        assert_eq!(release_tag(""), None);
    }

    #[test]
    fn a_first_run_shows_only_the_version_it_is_on() {
        let found = pending(&released(), "0.3.0", None);

        assert_eq!(texts(found), ["Three fixed", "Three added"]);
    }

    #[test]
    fn an_update_shows_what_changed_since_then_and_nothing_older() {
        let found = pending(&released(), "0.3.0", Some("0.2.0"));

        assert_eq!(texts(found), ["Three fixed", "Three added"]);
    }

    #[test]
    fn skipped_versions_are_combined_newest_first_under_the_current_one() {
        let found = pending(&released(), "0.3.0", Some("0.1.0")).unwrap();

        assert_eq!(found.version, "0.3.0");
        let texts: Vec<_> = found.changes.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["Three fixed", "Three added", "Two changed"]);
    }

    #[test]
    fn the_same_version_twice_shows_nothing() {
        assert_eq!(pending(&released(), "0.3.0", Some("0.3.0")), None);
    }

    #[test]
    fn a_release_with_nothing_to_list_shows_nothing() {
        let quiet = parse_log("tag: v0.3.0\tchore: hidden\ntag: v0.2.0\tfeat: older\n");

        assert_eq!(pending(&quiet, "0.3.0", Some("0.2.0")), None);
        assert_eq!(pending(&quiet, "0.3.0", None), None);
    }

    #[test]
    fn going_back_to_an_older_version_shows_nothing() {
        let older = parse_log("tag: v0.2.0\tfeat: two\n");

        assert_eq!(pending(&older, "0.2.0", Some("0.3.0")), None);
    }

    #[test]
    fn a_last_seen_version_that_cannot_be_read_counts_as_a_first_run() {
        let found = pending(&released(), "0.3.0", Some("ab12cd3"));

        assert_eq!(texts(found), ["Three fixed", "Three added"]);
    }

    #[test]
    fn a_development_build_lists_what_is_not_tagged_yet() {
        let current = "0.3.0-1-gab12cd3-dev";
        let first_run = pending(&parse_log(LOG), current, None);
        assert_eq!(texts(first_run), ["Not tagged yet"]);

        let after_a_release = pending(&parse_log(LOG), current, Some("0.3.0"));
        assert_eq!(texts(after_a_release), ["Not tagged yet"]);
    }

    #[test]
    fn version_ten_is_newer_than_version_nine() {
        // Compared as text, 0.10.0 sorts before 0.9.0 and the update shows nothing.
        let releases = parse_log("tag: v0.10.0\tfeat: ten\ntag: v0.9.0\tfeat: nine\n");

        assert_eq!(texts(pending(&releases, "0.10.0", Some("0.9.0"))), ["Ten"]);
    }

    #[test]
    fn a_development_build_does_not_repeat_itself_until_it_changes() {
        // Its untagged commits are always newer than any tag, so only the exact
        // version says it has been seen.
        let current = "0.3.0-1-gab12cd3-dev";
        assert_eq!(pending(&parse_log(LOG), current, Some(current)), None);
    }

    #[test]
    fn a_shown_version_only_counts_as_seen_once_closed() {
        let mut settings = Settings::default();

        assert!(check(&mut settings, &released(), "0.3.0").is_some());
        assert_eq!(settings.last_seen_version, None, "quitting now shows it again");

        dismiss(&mut settings, "0.3.0", false);
        assert_eq!(settings.last_seen_version.as_deref(), Some("0.3.0"));
        assert!(settings.show_whats_new);
        assert_eq!(check(&mut settings, &released(), "0.3.0"), None);
    }

    #[test]
    fn never_again_turns_it_off_and_only_then() {
        let mut settings = Settings::default();

        dismiss(&mut settings, "0.3.0", true);

        assert!(!settings.show_whats_new);
    }

    #[test]
    fn turned_off_it_shows_nothing_and_keeps_up_so_there_is_no_backlog_later() {
        let mut settings = Settings {
            show_whats_new: false,
            last_seen_version: Some("0.1.0".into()),
            ..Settings::default()
        };

        assert_eq!(check(&mut settings, &released(), "0.3.0"), None);
        assert_eq!(settings.last_seen_version.as_deref(), Some("0.3.0"));
    }

    #[test]
    fn a_version_with_nothing_to_show_counts_as_seen_right_away() {
        let mut settings = Settings {
            last_seen_version: Some("0.1.0".into()),
            ..Settings::default()
        };
        let quiet = parse_log("tag: v0.2.0\tchore: hidden\n");

        assert_eq!(check(&mut settings, &quiet, "0.2.0"), None);
        assert_eq!(settings.last_seen_version.as_deref(), Some("0.2.0"));
    }

    #[test]
    fn a_log_with_no_git_behind_it_shows_nothing() {
        assert_eq!(pending(&parse_log(""), "0.3.0", None), None);
    }
}
