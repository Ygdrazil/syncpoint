use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::connection::Connection;
use crate::template;

fn default_delete() -> bool {
    true
}

/// Remote paths that must never be a destination.
///
/// Deleting on the remote is on by default, so pointing a profile at one of
/// these would empty a system directory of everything not in the local folder.
/// Only exact matches are refused — `/home/deploy/app` is a normal target,
/// `/home` is not.
const PROTECTED_REMOTE_PATHS: &[&str] = &[
    "/", "~", "/bin", "/boot", "/dev", "/etc", "/home", "/lib", "/lib64", "/media",
    "/mnt", "/opt", "/proc", "/root", "/run", "/sbin", "/srv", "/sys", "/usr", "/var",
];

/// Strip trailing slashes for comparison, keeping bare `/` intact.
fn normalize_remote(path: &str) -> &str {
    let trimmed = path.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        "/"
    } else {
        trimmed
    }
}

fn default_debounce() -> u64 {
    500
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// Absolute path to the local directory that is watched and sent.
    pub source: String,
    /// Id of the [`Connection`] this profile sends through.
    #[serde(default)]
    pub connection_id: String,
    /// Path on the remote host. Absolute, or `~/...` for the remote home.
    pub remote_path: String,
    /// Whether the watcher runs for this profile.
    ///
    /// The UI calls this Pause and Resume rather than Enable and Disable: for
    /// one profile the effect is the same as Pause all — the watcher stops,
    /// and Sync now still works — so two words for it only invited the
    /// question of how they differed. Two things still do differ: Pause all
    /// leaves this flag alone so Resume all can put back exactly what was
    /// running, and Sync all skips a profile paused this way.
    #[serde(default)]
    pub enabled: bool,
    /// Make the remote an exact copy: anything not sent is removed there.
    ///
    /// Sends `--delete` *and* `--delete-excluded`, which have to travel
    /// together. rsync gives an `--exclude` two effects — hide the file on the
    /// sending side, and protect it from deletion on the receiving side — and
    /// with only `--delete` that second effect stops a directory that
    /// disappeared locally from ever being removed while the remote copy still
    /// holds anything excluded, reporting "cannot delete non-empty directory".
    /// Splitting the two into separate options only ever produced a setting
    /// that half worked.
    #[serde(default = "default_delete")]
    pub delete: bool,
    #[serde(default)]
    pub excludes: Vec<String>,
    /// Quiet period after the last filesystem event before rsync runs.
    #[serde(default = "default_debounce")]
    pub debounce_ms: u64,
    /// Containing folder, or `None` at the top level. Organisational only.
    #[serde(default)]
    pub folder_id: Option<String>,
    /// Place within `folder_id`, in an order shared with the folders that sit
    /// at the same level. See `folder::children`.
    #[serde(default)]
    pub position: u32,
}

impl Profile {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Name is required.".into());
        }
        // Left there by a template with nothing to put in it. Going ahead without
        // it could mean syncing onto the parent of what was meant, with delete on.
        for path in [&self.source, &self.remote_path] {
            if let Some(value) = template::unfilled(path) {
                return Err(format!(
                    "{path} still has {{{value}}} in it, and this profile has nothing to fill \
                     it with. Finish the path by hand."
                ));
            }
        }
        let source = Path::new(&self.source);
        if !source.is_absolute() {
            return Err("Source must be an absolute path.".into());
        }
        if !source.is_dir() {
            return Err(format!("Source is not a directory: {}", self.source));
        }
        if self.connection_id.trim().is_empty() {
            return Err("Pick a connection for this profile.".into());
        }
        let remote = self.remote_path.trim();
        if remote.is_empty() {
            return Err("Remote path is required.".into());
        }
        if !remote.starts_with('/') && !remote.starts_with("~/") {
            return Err("Remote path must start with / or ~/.".into());
        }
        // `.` and `..` are resolved on the remote host, not here, so without
        // this `/etc/..` walks straight past the list below and lands on `/`
        // with deletion armed.
        if remote
            .split('/')
            .any(|segment| segment == "." || segment == "..")
        {
            return Err("Remote path cannot contain . or .. segments.".into());
        }
        let normalized = normalize_remote(remote);
        if PROTECTED_REMOTE_PATHS.contains(&normalized) {
            return Err(format!(
                "Refusing to sync onto {normalized}: that is a system directory, and this \
                 profile would delete everything in it that is not in the local folder. Use a \
                 subdirectory such as {}/syncpoint.",
                normalized.trim_end_matches('/')
            ));
        }
        if !(50..=60_000).contains(&self.debounce_ms) {
            return Err("Debounce must be between 50 and 60000 ms.".into());
        }
        Ok(())
    }

    /// Source with a trailing slash, so rsync copies the folder *contents* into
    /// the remote path instead of nesting the folder one level deeper.
    pub fn source_arg(&self) -> String {
        format!("{}/", self.source.trim_end_matches('/'))
    }

    pub fn dest_arg(&self, connection: &Connection) -> String {
        format!(
            "{}:{}",
            connection.target(),
            quote_remote(self.remote_path.trim())
        )
    }
}

/// rsync hands the remote path to a shell on the remote host, so it has to be
/// quoted or paths with spaces get split into separate arguments. `~/` is left
/// outside the quotes so it still expands to the remote home directory.
fn quote_remote(path: &str) -> String {
    let path = path.trim_end_matches('/');
    match path.strip_prefix("~/") {
        Some(rest) => format!("~/{}", single_quote(rest)),
        None => single_quote(path),
    }
}

fn single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection() -> Connection {
        Connection {
            id: "c1".into(),
            name: "prod".into(),
            user: "volodia".into(),
            host: "example.com".into(),
            port: 22,
            identity_file: String::new(),
        }
    }

    fn profile() -> Profile {
        Profile {
            id: "1".into(),
            name: "test".into(),
            source: "/tmp/src".into(),
            connection_id: "c1".into(),
            remote_path: "/srv/data".into(),
            enabled: false,
            delete: true,
            excludes: vec![],
            debounce_ms: 500,
            folder_id: None,
            position: 0,
        }
    }

    #[test]
    fn a_profile_saved_with_mirror_mode_carries_straight_over() {
        // The merge kept the same field name, so there is no migration step to
        // run: a profile that already deleted on the remote simply keeps doing so, and
        // now gets the excluded files deleted too. The interim, separate
        // `deleteExcluded` is ignored rather than being an error.
        let raw = r#"{
          "id": "a", "name": "Alpha", "source": "/tmp/a",
          "connectionId": "c", "remotePath": "/srv/a",
          "enabled": true, "delete": true, "deleteExcluded": false,
          "excludes": [".git"], "debounceMs": 500
        }"#;
        let profile: Profile = serde_json::from_str(raw).expect("an existing profile still loads");
        assert!(profile.delete);

        // And one that never had it stays off.
        let off = raw.replace("\"delete\": true", "\"delete\": false");
        let profile: Profile = serde_json::from_str(&off).unwrap();
        assert!(!profile.delete);
    }

    #[test]
    fn a_path_still_waiting_on_a_template_value_is_refused() {
        let mut waiting = profile();
        waiting.remote_path = "/home/code/dev/{slug}".into();
        let error = waiting.validate().unwrap_err();
        assert!(error.contains("{slug}"), "{error}");

        let mut waiting = profile();
        waiting.source = "/Users/me/{folder}/x".into();
        let error = waiting.validate().unwrap_err();
        assert!(error.contains("{folder}"), "{error}");
    }

    #[test]
    fn braces_that_are_not_a_template_value_are_just_a_folder_name() {
        let mut odd = profile();
        odd.source = std::env::temp_dir().to_string_lossy().into_owned();
        odd.remote_path = "/data/{other}/x".into();
        assert!(odd.validate().is_ok());
    }

    #[test]
    fn a_remote_path_cannot_walk_out_of_itself() {
        // validate() also requires the source to exist; without a real one
        // every assertion below would pass for the wrong reason.
        let source = std::env::temp_dir().join("syncpoint-traversal-test");
        std::fs::create_dir_all(&source).unwrap();
        let sound = || {
            let mut p = profile();
            p.source = source.display().to_string();
            p
        };

        // `..` is resolved on the remote host, not here, so /etc/.. is not in
        // PROTECTED_REMOTE_PATHS and used to pass — landing rsync on / with
        // deletion armed.
        for path in ["/etc/..", "/srv/../..", "~/../..", "/srv/./data", "/usr/local/../.."] {
            let mut p = sound();
            p.remote_path = path.into();
            assert!(p.validate().is_err(), "{path} should be refused");
        }

        // Dots inside a name are not path segments and stay allowed.
        for path in ["/srv/my.app/..data", "/srv/apps", "~/dev/site"] {
            let mut p = sound();
            p.remote_path = path.into();
            assert!(p.validate().is_ok(), "should allow {path}: {:?}", p.validate());
        }

        let _ = std::fs::remove_dir_all(&source);
    }

    #[test]
    fn source_always_gets_one_trailing_slash() {
        let mut p = profile();
        assert_eq!(p.source_arg(), "/tmp/src/");
        p.source = "/tmp/src/".into();
        assert_eq!(p.source_arg(), "/tmp/src/");
    }

    #[test]
    fn dest_quotes_remote_path() {
        let mut p = profile();
        let mut c = connection();
        assert_eq!(p.dest_arg(&c), "volodia@example.com:'/srv/data'");

        p.remote_path = "/srv/my data".into();
        assert_eq!(p.dest_arg(&c), "volodia@example.com:'/srv/my data'");

        p.remote_path = "~/my data".into();
        assert_eq!(p.dest_arg(&c), "volodia@example.com:~/'my data'");

        c.user = "".into();
        p.remote_path = "/srv/data".into();
        assert_eq!(p.dest_arg(&c), "example.com:'/srv/data'");
    }

    #[test]
    fn dest_escapes_single_quotes() {
        let mut p = profile();
        p.remote_path = "/srv/it's".into();
        assert_eq!(p.dest_arg(&connection()), r"volodia@example.com:'/srv/it'\''s'");
    }

    #[test]
    fn system_directories_are_refused_as_destinations() {
        for path in [
            "/", "/home", "/usr", "/etc", "/var", "/root", "~",
            // Trailing slashes must not sneak one past the check.
            "/home/", "/usr///", "~/",
        ] {
            let mut p = profile();
            p.remote_path = path.into();
            assert!(p.validate().is_err(), "should refuse {path:?}");
        }
    }

    #[test]
    fn subdirectories_of_system_paths_are_allowed() {
        // validate() also requires the source to exist, so give it a real one.
        let source = std::env::temp_dir().join("syncpoint-profile-test");
        std::fs::create_dir_all(&source).unwrap();

        // These are ordinary deploy targets; only the roots themselves are out.
        for path in ["/home/deploy/app", "/usr/local/share/app", "/srv/apps", "~/dev/site"] {
            let mut p = profile();
            p.source = source.display().to_string();
            p.remote_path = path.into();
            assert!(p.validate().is_ok(), "should allow {path:?}: {:?}", p.validate());
        }
        let _ = std::fs::remove_dir_all(&source);
    }

    #[test]
    fn validate_rejects_dangerous_or_incomplete_config() {
        let mut p = profile();
        p.remote_path = "/".into();
        assert!(p.validate().is_err());

        let mut p = profile();
        p.remote_path = "relative/path".into();
        assert!(p.validate().is_err());

        let mut p = profile();
        p.connection_id = "".into();
        assert!(p.validate().is_err());

        let mut p = profile();
        p.source = "relative".into();
        assert!(p.validate().is_err());
    }
}
