//! Locating an rsync binary and working out what it supports.

use std::path::Path;

use serde::Serialize;

/// Checked in order; the first non-openrsync 3.x wins.
const CANDIDATES: &[&str] = &[
    "/opt/homebrew/bin/rsync",
    "/usr/local/bin/rsync",
    "/usr/bin/rsync",
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RsyncInfo {
    pub path: String,
    pub version_line: String,
    /// macOS ships Apple's openrsync, which only speaks protocol 29 and lacks
    /// most rsync 3.x flags. Worth surfacing so the limitation is visible.
    pub is_openrsync: bool,
    pub major: u32,
    /// `--info=progress2` is rsync 3.1+, so openrsync and Apple's 2.6.9-era
    /// build do without. Derived once here so the rule lives in one place and
    /// the UI can read it directly.
    pub supports_progress: bool,
    /// rsync 3.2.4+ escapes shell-active characters in remote args itself. This
    /// app quotes the remote path already, so without opting out the quotes
    /// arrive literally and the destination becomes a directory named `'...'`
    /// under the remote home. Detected from `--help` rather than a version
    /// comparison, since the flag's presence is the thing that matters.
    pub protects_args: bool,
}

/// Classify a binary from its `--version` banner. Split out from [`probe`] so
/// the parsing is testable without a binary on disk.
fn describe(path: &str, version_line: &str, help: &str) -> RsyncInfo {
    let is_openrsync = version_line.to_lowercase().contains("openrsync");

    // rsync 3.x: "rsync  version 3.4.1  protocol version 32"
    // openrsync: "openrsync: protocol version 29"
    let parsed = version_line
        .split_whitespace()
        .skip_while(|t| *t != "version")
        .nth(1)
        .and_then(|v| v.split('.').next())
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    // openrsync's banner reports the *protocol* version, not a release, so it
    // must never be read as a major version.
    let major = if is_openrsync { 2 } else { parsed };

    RsyncInfo {
        path: path.to_string(),
        version_line: version_line.to_string(),
        is_openrsync,
        major,
        supports_progress: !is_openrsync && major >= 3,
        protects_args: help.contains("--old-args"),
    }
}

fn probe(path: &str) -> Option<RsyncInfo> {
    if !Path::new(path).exists() {
        return None;
    }
    let out = std::process::Command::new(path).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);

    // Capability probe: `--old-args` exists only on builds that protect args.
    let help = std::process::Command::new(path)
        .arg("--help")
        .output()
        .map(|o| format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
        .unwrap_or_default();

    Some(describe(path, text.lines().next()?.trim(), &help))
}

pub fn detect() -> Option<RsyncInfo> {
    let mut found: Vec<RsyncInfo> = CANDIDATES.iter().filter_map(|c| probe(c)).collect();
    // Real rsync before openrsync; among those, highest major version first.
    found.sort_by_key(|i| (i.is_openrsync, std::cmp::Reverse(i.major)));
    found.into_iter().next()
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// Trimmed from the real `--help` of rsync 3.4.4.
    pub(in crate::rsync) const MODERN_HELP: &str = "--old-args               disable the modern arg-protection idiom\n\
                               --secluded-args, -s      use the protocol to safely send the args";

    pub(in crate::rsync) fn modern() -> RsyncInfo {
        describe(
            "/opt/homebrew/bin/rsync",
            "rsync  version 3.4.1  protocol version 32",
            MODERN_HELP,
        )
    }

    pub(in crate::rsync) fn openrsync() -> RsyncInfo {
        describe("/usr/bin/rsync", "openrsync: protocol version 29", "")
    }

    #[test]
    fn version_banners_are_classified_correctly() {
        let modern = modern();
        assert!(!modern.is_openrsync);
        assert_eq!(modern.major, 3);
        assert!(modern.supports_progress);

        // openrsync's banner says "protocol version 29"; reading 29 as a major
        // version would wrongly mark it as progress-capable.
        let old = openrsync();
        assert!(old.is_openrsync);
        assert_eq!(old.major, 2);
        assert!(!old.supports_progress);

        // rsync 2.6.9 is real rsync, but still predates --info.
        let ancient = describe("/usr/bin/rsync", "rsync  version 2.6.9  protocol version 29", "");
        assert!(!ancient.is_openrsync);
        assert_eq!(ancient.major, 2);
        assert!(!ancient.supports_progress);

        // An unreadable banner must degrade to the safe baseline.
        let unknown = describe("/usr/bin/rsync", "some unexpected banner", "");
        assert_eq!(unknown.major, 0);
        assert!(!unknown.supports_progress);
    }

    #[test]
    fn arg_protection_is_detected_from_help_not_the_version() {
        assert!(modern().protects_args);
        assert!(!openrsync().protects_args);
        // A 3.x build predating the change advertises no such flag.
        assert!(!describe("/x", "rsync  version 3.1.3  protocol version 31", "").protects_args);
    }
}
