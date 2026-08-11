use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::connection::Connection;
use crate::profile::Profile;
use crate::store::Config;

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
    pub profiles: Vec<Profile>,
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
}

pub fn export(config: &Config, include_connections: bool, exported_at: u64) -> Bundle {
    Bundle {
        version: BUNDLE_VERSION,
        exported_at,
        connections: if include_connections {
            config.connections.clone()
        } else {
            Vec::new()
        },
        profiles: config.profiles.clone(),
    }
}

/// Merge a bundle into the existing config, matching on id.
///
/// Merging rather than replacing means importing onto a populated app adds to
/// it instead of wiping it. Imported profiles always arrive **disabled**: an
/// import should never silently start pushing files to someone else's server.
pub fn import(config: &mut Config, bundle: Bundle) -> Result<ImportSummary, String> {
    if bundle.version > BUNDLE_VERSION {
        return Err(format!(
            "This file was written by a newer version of SyncPoint (format {}, this build \
             understands {BUNDLE_VERSION}).",
            bundle.version
        ));
    }

    let mut summary = ImportSummary::default();

    for incoming in bundle.connections {
        match config.connections.iter_mut().find(|c| c.id == incoming.id) {
            Some(existing) => {
                *existing = incoming;
                summary.connections_updated += 1;
            }
            None => {
                config.connections.push(incoming);
                summary.connections_added += 1;
            }
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
        }
    }

    fn config() -> Config {
        Config {
            settings: Settings::default(),
            connections: vec![connection("c1")],
            profiles: vec![profile("p1", "c1")],
        }
    }

    #[test]
    fn connections_are_left_out_unless_requested() {
        let bundle = export(&config(), false, 0);
        assert!(bundle.connections.is_empty());
        assert_eq!(bundle.profiles.len(), 1);

        let with = export(&config(), true, 0);
        assert_eq!(with.connections.len(), 1);
    }

    #[test]
    fn a_bundle_round_trips_through_json() {
        let bundle = export(&config(), true, 1_700_000_000);
        let json = serde_json::to_string_pretty(&bundle).unwrap();
        let back: Bundle = serde_json::from_str(&json).unwrap();

        assert_eq!(back.version, BUNDLE_VERSION);
        assert_eq!(back.exported_at, 1_700_000_000);
        assert_eq!(back.profiles[0].name, "profile-p1");
        assert_eq!(back.connections[0].host, "example.com");
    }

    #[test]
    fn importing_merges_rather_than_replacing() {
        let mut target = config();
        let incoming = Bundle {
            version: BUNDLE_VERSION,
            exported_at: 0,
            connections: vec![connection("c2")],
            profiles: vec![profile("p2", "c2")],
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
                connections: vec![],
                profiles: vec![renamed],
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
                connections: vec![connection("c1")],
                // Exported while running, so enabled in the file.
                profiles: vec![profile("p1", "c1")],
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
                // Exported without connections, so p1 points at nothing.
                connections: vec![],
                profiles: vec![profile("p1", "c1")],
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
                connections: vec![],
                profiles: vec![profile("p1", "c1")],
            },
        );
        assert!(result.is_err());
        assert!(target.profiles.is_empty(), "nothing may be applied on refusal");
    }
}
