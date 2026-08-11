use std::path::Path;
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// Reuse one SSH connection across syncs. Without this every rsync run pays a
/// fresh TCP + key exchange (~0.5-1s), which dwarfs the transfer itself for the
/// small, frequent syncs a file watcher produces.
///
/// `%C` is a short hash of (local host, remote host, port, user), which keeps
/// the socket path well under the ~104 byte unix socket limit. `~/.ssh` is used
/// rather than a world-writable directory like /tmp.
const CONTROL_PATH: &str = "~/.ssh/syncpoint-cm-%C";
const CONTROL_PERSIST_SECS: u32 = 300;

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
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Connection name is required.".into());
        }
        let host = self.host.trim();
        if host.is_empty() {
            return Err("Host is required.".into());
        }
        if host.split_whitespace().count() > 1 {
            return Err("Host cannot contain spaces.".into());
        }
        if self.user.split_whitespace().count() > 1 {
            return Err("User cannot contain spaces.".into());
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
            if !key.starts_with('~') && !Path::new(key).exists() {
                return Err(format!("Key file not found: {key}"));
            }
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
        args.push(format!("ControlPath={CONTROL_PATH}"));
        args.push("-o".to_string());
        args.push(format!("ControlPersist={CONTROL_PERSIST_SECS}"));
        // A missing key or unknown host fails immediately with a readable
        // error instead of blocking forever on a prompt nobody can see.
        args.push("-o".to_string());
        args.push("BatchMode=yes".to_string());
        args
    }

    /// The full command for rsync's `-e`, as a single argv element.
    pub fn ssh_command(&self) -> String {
        format!("ssh {}", self.ssh_args().join(" "))
    }
}

/// Open a real SSH session and run `true`. Doubles as a warm-up: it primes the
/// ControlMaster socket, so the next sync skips the handshake.
pub async fn test(connection: &Connection) -> Result<String, String> {
    connection.validate()?;

    let mut args = connection.ssh_args();
    args.push("-o".to_string());
    args.push("ConnectTimeout=10".to_string());
    args.push(connection.target());
    args.push("true".to_string());

    let output = Command::new("ssh")
        .args(&args)
        .stdin(Stdio::null())
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
    fn ssh_command_always_carries_batch_mode_and_connection_reuse() {
        let command = connection().ssh_command();
        assert!(command.starts_with("ssh -p 2222 "));
        assert!(command.contains("BatchMode=yes"));
        assert!(command.contains("ControlMaster=auto"));
        assert!(command.contains("ControlPersist="));
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
