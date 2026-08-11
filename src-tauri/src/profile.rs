use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::connection::Connection;

fn default_delete() -> bool {
    true
}

/// Remote paths that must never be a destination.
///
/// Mirror mode is on by default, so pointing a profile at one of these would
/// delete everything in a system directory that is not in the local folder.
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
    #[serde(default)]
    pub enabled: bool,
    /// Mirror mode: pass `--delete` so removals propagate to the remote.
    #[serde(default = "default_delete")]
    pub delete: bool,
    #[serde(default)]
    pub excludes: Vec<String>,
    /// Quiet period after the last filesystem event before rsync runs.
    #[serde(default = "default_debounce")]
    pub debounce_ms: u64,
}

impl Profile {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Name is required.".into());
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
        let normalized = normalize_remote(remote);
        if PROTECTED_REMOTE_PATHS.contains(&normalized) {
            return Err(format!(
                "Refusing to sync onto {normalized}: that is a system directory, and mirror \
                 mode would delete everything in it. Use a subdirectory such as \
                 {}/syncpoint.",
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
        }
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
