use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::time::MissedTickBehavior;

use crate::connection::Connection;
use crate::profile::Profile;

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

pub fn build_args(profile: &Profile, connection: &Connection, rsync: &RsyncInfo) -> Vec<String> {
    // The baseline is flags that exist in rsync 2.6.9, so the same command
    // works on Apple's openrsync; newer capabilities are added only when the
    // detected binary actually has them.
    let mut args = vec![
        "-a".to_string(), // archive: recurse, preserve perms/times/symlinks
        "-z".to_string(), // compress in flight; worth it over ssh
        "-v".to_string(), // per-file lines, which become the log output
        "-h".to_string(),
    ];
    if rsync.protects_args {
        // Keep one quoting behaviour across every rsync: this app escapes the
        // remote path itself, which rsync 3.2.4+ would otherwise double up on.
        args.push("--old-args".to_string());
    }
    if rsync.supports_progress {
        args.push("--info=progress2".to_string());
        // Progress arrives as carriage-return updates, which rsync buffers by
        // default when its output is a pipe rather than a terminal.
        args.push("--outbuf=L".to_string());
    }
    if profile.delete {
        args.push("--delete".to_string());
    }
    for pattern in &profile.excludes {
        let pattern = pattern.trim();
        if !pattern.is_empty() {
            args.push(format!("--exclude={pattern}"));
        }
    }
    args.push("-e".to_string());
    args.push(connection.ssh_command());
    args.push(profile.source_arg());
    args.push(profile.dest_arg(connection));
    args
}

/// How often batched output is pushed to the frontend.
const FLUSH_INTERVAL_MS: u64 = 100;
/// Ceiling on lines carried by one batch. `rsync -v` prints a line per file, so
/// a first sync of a large tree produces tens of thousands of them. The UI keeps
/// only a bounded scrollback, so shipping more than this per flush is wasted
/// work on both sides of the IPC boundary — and enough of it freezes the
/// webview's main thread. Dropped lines are counted, never silently lost: the
/// complete transcript still goes to stderr.
const MAX_BATCH_LINES: usize = 400;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LogLine {
    line: String,
    stream: &'static str,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LogBatch {
    profile_id: String,
    lines: Vec<LogLine>,
    /// Lines dropped from this batch to keep the payload bounded.
    skipped: usize,
}

/// Collect output from both streams and emit it in periodic batches rather than
/// one event per line. Returns the sender the readers push into; dropping every
/// clone of it ends the task after a final flush.
fn spawn_log_flusher(app: AppHandle, profile_id: String) -> UnboundedSender<LogLine> {
    let (tx, mut rx) = mpsc::unbounded_channel::<LogLine>();

    tauri::async_runtime::spawn(async move {
        let mut pending: Vec<LogLine> = Vec::new();
        let mut skipped = 0usize;
        let mut ticker = tokio::time::interval(Duration::from_millis(FLUSH_INTERVAL_MS));
        // A stalled flush must not cause a burst of catch-up ticks.
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                incoming = rx.recv() => match incoming {
                    Some(line) => push_bounded(&mut pending, &mut skipped, line),
                    None => break,
                },
                _ = ticker.tick() => flush(&app, &profile_id, &mut pending, &mut skipped),
            }
        }
        flush(&app, &profile_id, &mut pending, &mut skipped);
    });

    tx
}

/// Append a line, discarding the oldest once the batch is full so the newest
/// output always survives. Whatever is dropped is counted, not lost silently.
fn push_bounded(pending: &mut Vec<LogLine>, skipped: &mut usize, line: LogLine) {
    pending.push(line);
    if pending.len() > MAX_BATCH_LINES {
        let overflow = pending.len() - MAX_BATCH_LINES;
        pending.drain(..overflow);
        *skipped += overflow;
    }
}

fn flush(app: &AppHandle, profile_id: &str, pending: &mut Vec<LogLine>, skipped: &mut usize) {
    if pending.is_empty() && *skipped == 0 {
        return;
    }
    let _ = app.emit(
        "sync-log",
        LogBatch {
            profile_id: profile_id.to_string(),
            lines: std::mem::take(pending),
            skipped: std::mem::take(skipped),
        },
    );
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncProgress {
    profile_id: String,
    percent: u8,
    transferred: String,
    rate: String,
    eta: String,
}

/// Splits output on carriage returns as well as newlines.
///
/// `--info=progress2` redraws one line by emitting `\r`, so a newline-only
/// reader would accumulate the whole transfer into a single unbounded line and
/// deliver it once, at the end — exactly when it is no longer progress.
#[derive(Default)]
struct SegmentSplitter {
    pending: Vec<u8>,
}

impl SegmentSplitter {
    fn feed(&mut self, bytes: &[u8], mut emit: impl FnMut(String)) {
        for &byte in bytes {
            if byte == b'\n' || byte == b'\r' {
                self.take(&mut emit);
            } else {
                self.pending.push(byte);
            }
        }
    }

    fn flush(&mut self, mut emit: impl FnMut(String)) {
        self.take(&mut emit);
    }

    fn take(&mut self, emit: &mut impl FnMut(String)) {
        if self.pending.is_empty() {
            return;
        }
        let segment = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        if !segment.trim().is_empty() {
            emit(segment);
        }
    }
}

/// Recognise an `--info=progress2` line, e.g.
/// `    1.31G  42%   11.90MB/s    0:00:38 (xfr#12, to-chk=99/512)`.
///
/// Returns None for ordinary output, including filenames containing a `%`,
/// which is why the token before the percentage must look like a size.
fn parse_progress(line: &str) -> Option<(u8, String, String, String)> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let at = fields.iter().position(|f| f.ends_with('%'))?;
    if at == 0 {
        return None;
    }
    let size = fields[at - 1];
    if !size.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let percent: u8 = fields[at].trim_end_matches('%').parse().ok()?;
    if percent > 100 {
        return None;
    }
    Some((
        percent,
        size.to_string(),
        fields.get(at + 1).copied().unwrap_or_default().to_string(),
        fields.get(at + 2).copied().unwrap_or_default().to_string(),
    ))
}

/// Read a stream, splitting on newlines and carriage returns.
async fn read_segments<R: AsyncRead + Unpin>(reader: R, mut emit: impl FnMut(String)) {
    let mut reader = BufReader::new(reader);
    let mut chunk = [0u8; 8192];
    let mut splitter = SegmentSplitter::default();
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => splitter.feed(&chunk[..n], &mut emit),
        }
    }
    splitter.flush(&mut emit);
}

fn log(tx: &UnboundedSender<LogLine>, profile_id: &str, stream: &'static str, line: String) {
    // Mirrored to stderr as well as the UI, so a full transcript can be
    // captured to a file for bug reports. See "Getting logs" in the README.
    eprintln!("[syncpoint {profile_id} {stream}] {line}");
    let _ = tx.send(LogLine { line, stream });
}

/// Run one rsync pass, streaming its output to the frontend as it arrives.
pub async fn run(
    app: &AppHandle,
    profile: &Profile,
    connection: &Connection,
    rsync: &RsyncInfo,
) -> Result<String, String> {
    let bin = &rsync.path;
    let args = build_args(profile, connection, rsync);
    let tx = spawn_log_flusher(app.clone(), profile.id.clone());
    log(&tx, &profile.id, "meta", format!("$ {bin} {}", args.join(" ")));

    let mut child = Command::new(bin)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Could not start {bin}: {e}"))?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    let mut stderr_reader = BufReader::new(stderr).lines();

    // stderr is collected as well as streamed, so a failure can report the
    // actual reason rather than just an exit code.
    let tx_err = tx.clone();
    let id_err = profile.id.clone();
    let stderr_task = tauri::async_runtime::spawn(async move {
        let mut collected = Vec::new();
        while let Ok(Some(line)) = stderr_reader.next_line().await {
            log(&tx_err, &id_err, "stderr", line.clone());
            collected.push(line);
        }
        collected
    });

    let mut files = 0usize;
    read_segments(stdout, |line| {
        // Progress redraws many times a second; sending it to the log would
        // drown the file list it is meant to accompany.
        if let Some((percent, transferred, rate, eta)) = parse_progress(&line) {
            let _ = app.emit(
                "sync-progress",
                SyncProgress {
                    profile_id: profile.id.clone(),
                    percent,
                    transferred,
                    rate,
                    eta,
                },
            );
            return;
        }
        if !line.starts_with("sending ") && !line.starts_with("sent ") {
            files += 1;
        }
        log(&tx, &profile.id, "stdout", line);
    })
    .await;

    let status = child
        .wait()
        .await
        .map_err(|e| format!("rsync did not finish: {e}"))?;
    let stderr_lines = stderr_task.await.unwrap_or_default();

    // Closing the last sender lets the flusher emit its tail and finish.
    drop(tx);

    match status.code() {
        Some(0) => Ok(format!("{files} item(s) transferred")),
        // 24 = source files vanished mid-transfer. Normal for a live folder
        // being watched, and the next run picks up whatever is current.
        Some(24) => Ok(format!("{files} item(s) transferred (some vanished mid-sync)")),
        Some(code) => Err(explain(code, &stderr_lines)),
        None => Err("rsync was terminated by a signal.".to_string()),
    }
}

/// rsync's last stderr line is always its generic trailer
/// (`rsync error: ... at main.c(791)`), which names the error class but never
/// the cause. The line before it is the one that says what actually happened,
/// so the trailer is skipped whenever anything more specific was printed.
fn most_useful_lines(stderr: &[String]) -> Vec<&String> {
    let meaningful: Vec<&String> = stderr
        .iter()
        .filter(|l| !l.trim().is_empty())
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with("rsync error:") && !t.starts_with("rsync warning:")
        })
        .collect();

    // Fall back to the trailer only when it was the sole output.
    if meaningful.is_empty() {
        stderr.iter().filter(|l| !l.trim().is_empty()).rev().take(1).collect()
    } else {
        // The tail carries the failure; earlier lines are usually per-file noise.
        meaningful.into_iter().rev().take(2).rev().collect()
    }
}

fn explain(code: i32, stderr: &[String]) -> String {
    let hint = match code {
        1 => "rsync usage error — check the excludes and paths.",
        2 => "Protocol mismatch between local and remote rsync.",
        11 => "The remote side could not read or write the destination. Usually the remote \
               directory does not exist, its parent is missing, or the SSH user cannot write \
               there.",
        12 => "Protocol error, usually a failed SSH session or no rsync on the remote host.",
        13 => "Permission denied on the remote side.",
        23 => "Partial transfer — some files could not be sent (permissions?).",
        30 => "Timed out waiting on the remote host.",
        255 => "SSH connection failed. With BatchMode on, key auth must already work: try `ssh <user>@<host>` once by hand to accept the host key and confirm the key is loaded.",
        _ => "rsync failed.",
    };

    let detail = most_useful_lines(stderr)
        .into_iter()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join(" / ");

    if detail.is_empty() {
        format!("{hint} (exit {code})")
    } else {
        format!("{hint} (exit {code}) — {detail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from the real `--help` of rsync 3.4.4.
    const MODERN_HELP: &str = "--old-args               disable the modern arg-protection idiom\n\
                               --secluded-args, -s      use the protocol to safely send the args";

    fn modern() -> RsyncInfo {
        describe(
            "/opt/homebrew/bin/rsync",
            "rsync  version 3.4.1  protocol version 32",
            MODERN_HELP,
        )
    }

    fn openrsync() -> RsyncInfo {
        describe("/usr/bin/rsync", "openrsync: protocol version 29", "")
    }

    fn connection() -> Connection {
        Connection {
            id: "c1".into(),
            name: "prod".into(),
            user: "volodia".into(),
            host: "example.com".into(),
            port: 2222,
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
            excludes: vec![".git".into(), "  ".into(), "node_modules".into()],
            debounce_ms: 500,
        }
    }

    #[test]
    fn builds_expected_argv() {
        let args = build_args(&profile(), &connection(), &openrsync());
        assert_eq!(&args[0..4], &["-a", "-z", "-v", "-h"]);
        assert!(args.contains(&"--delete".to_string()));
        assert!(args.contains(&"--exclude=.git".to_string()));
        assert!(args.contains(&"--exclude=node_modules".to_string()));
        // The blank exclude must not become `--exclude=`, which would be a
        // pattern matching every path.
        assert!(!args.iter().any(|a| a == "--exclude="));
        assert_eq!(args[args.len() - 2], "/tmp/src/");
        assert_eq!(args[args.len() - 1], "volodia@example.com:'/srv/data'");
    }

    #[test]
    fn delete_flag_is_opt_out() {
        let mut p = profile();
        p.delete = false;
        assert!(!build_args(&p, &connection(), &openrsync()).contains(&"--delete".to_string()));
    }

    #[test]
    fn ssh_command_is_a_single_unquoted_argument() {
        let args = build_args(&profile(), &connection(), &openrsync());
        let idx = args.iter().position(|a| a == "-e").unwrap();
        let ssh = &args[idx + 1];
        assert!(ssh.starts_with("ssh -p 2222 "));
        assert!(ssh.contains("ControlMaster=auto"));
        assert!(ssh.contains("BatchMode=yes"));
        assert!(!ssh.contains('\''), "quotes would be passed through literally");
    }

    #[test]
    fn batches_stay_bounded_and_keep_the_newest_lines() {
        let mut pending = Vec::new();
        let mut skipped = 0usize;

        // Far more output than one batch can carry, as a big first sync produces.
        let total = MAX_BATCH_LINES * 25;
        for i in 0..total {
            push_bounded(
                &mut pending,
                &mut skipped,
                LogLine {
                    line: format!("file-{i}"),
                    stream: "stdout",
                },
            );
            assert!(pending.len() <= MAX_BATCH_LINES, "batch grew past its cap");
        }

        assert_eq!(pending.len(), MAX_BATCH_LINES);
        // Nothing is unaccounted for: kept + dropped == everything pushed.
        assert_eq!(pending.len() + skipped, total);
        // The tail is what survives, so the UI shows the most recent output.
        assert_eq!(pending.last().unwrap().line, format!("file-{}", total - 1));
        assert_eq!(
            pending.first().unwrap().line,
            format!("file-{}", total - MAX_BATCH_LINES)
        );
    }

    #[test]
    fn small_batches_drop_nothing() {
        let mut pending = Vec::new();
        let mut skipped = 0usize;
        for i in 0..MAX_BATCH_LINES {
            push_bounded(
                &mut pending,
                &mut skipped,
                LogLine {
                    line: format!("file-{i}"),
                    stream: "stdout",
                },
            );
        }
        assert_eq!(skipped, 0);
        assert_eq!(pending.len(), MAX_BATCH_LINES);
        assert_eq!(pending[0].line, "file-0");
    }

    #[test]
    fn progress_flags_only_appear_on_a_capable_rsync() {
        let modern_args = build_args(&profile(), &connection(), &modern());
        assert!(modern_args.contains(&"--info=progress2".to_string()));
        assert!(modern_args.contains(&"--outbuf=L".to_string()));

        // openrsync would reject both, so the baseline stays 2.6.9-safe.
        let old_args = build_args(&profile(), &connection(), &openrsync());
        assert!(!old_args.iter().any(|a| a.starts_with("--info")));
        assert!(!old_args.iter().any(|a| a.starts_with("--outbuf")));
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

    fn collect(chunks: &[&str]) -> Vec<String> {
        let mut splitter = SegmentSplitter::default();
        let mut out = Vec::new();
        for chunk in chunks {
            splitter.feed(chunk.as_bytes(), |s| out.push(s));
        }
        splitter.flush(|s| out.push(s));
        out
    }

    #[test]
    fn carriage_returns_split_segments_like_newlines() {
        // Progress redraws with \r; without this every update would pile into
        // one line delivered only once the transfer had finished.
        assert_eq!(collect(&["a\rb\rc"]), vec!["a", "b", "c"]);
        assert_eq!(collect(&["one\ntwo\n"]), vec!["one", "two"]);
        // \r\n must not produce a phantom empty segment.
        assert_eq!(collect(&["one\r\ntwo\r\n"]), vec!["one", "two"]);
    }

    #[test]
    fn segments_spanning_chunk_boundaries_are_rejoined() {
        assert_eq!(collect(&["par", "tial\rnext"]), vec!["partial", "next"]);
        // A trailing segment with no delimiter still arrives, via flush.
        assert_eq!(collect(&["no delimiter"]), vec!["no delimiter"]);
    }

    #[test]
    fn parses_a_real_progress2_line() {
        let (percent, transferred, rate, eta) =
            parse_progress("      1.31G  42%   11.90MB/s    0:00:38 (xfr#12, to-chk=99/512)")
                .expect("recognised as progress");
        assert_eq!(percent, 42);
        assert_eq!(transferred, "1.31G");
        assert_eq!(rate, "11.90MB/s");
        assert_eq!(eta, "0:00:38");

        assert_eq!(parse_progress("    0  0%    0.00kB/s    0:00:00").unwrap().0, 0);
        assert_eq!(parse_progress("  4.00M 100%    3.81MB/s    0:00:01").unwrap().0, 100);
    }

    #[test]
    fn ordinary_output_is_not_mistaken_for_progress() {
        // These all reach the log instead.
        for line in [
            "src/main.rs",
            "reports/100% coverage.txt",
            "sending incremental file list",
            "sent 3,697,000 bytes  received 480 bytes  8,198,000.00 bytes/sec",
            "",
        ] {
            assert!(parse_progress(line).is_none(), "wrongly parsed: {line:?}");
        }
    }

    #[test]
    fn quoting_is_opted_out_of_only_where_rsync_would_double_it() {
        // rsync 3.2.4+ escapes remote args itself; combined with this app's own
        // quoting the destination became a literal "'/path'" under the home
        // directory. --old-args hands escaping back to us.
        let modern_args = build_args(&profile(), &connection(), &modern());
        assert!(modern_args.contains(&"--old-args".to_string()));

        // openrsync has no such flag and relies on the remote shell stripping
        // the quotes, so it must not be passed.
        let old_args = build_args(&profile(), &connection(), &openrsync());
        assert!(!old_args.contains(&"--old-args".to_string()));
    }

    #[test]
    fn arg_protection_is_detected_from_help_not_the_version() {
        assert!(modern().protects_args);
        assert!(!openrsync().protects_args);
        // A 3.x build predating the change advertises no such flag.
        assert!(!describe("/x", "rsync  version 3.1.3  protocol version 31", "").protects_args);
    }

    #[test]
    fn the_cause_is_reported_not_just_rsyncs_trailer() {
        // Real output: the trailer names the error class, the line above it
        // names the cause. Reporting only the trailer is what made an exit 11
        // unactionable.
        let stderr = vec![
            "rsync: [Receiver] mkdir \"/srv/apps/site\" failed: Permission denied (13)".to_string(),
            "rsync error: error in file IO (code 11) at main.c(791) [Receiver=3.2.7]".to_string(),
        ];
        let message = explain(11, &stderr);
        assert!(message.contains("Permission denied"), "cause missing from: {message}");
        assert!(message.contains("exit 11"));
    }

    #[test]
    fn the_trailer_is_still_shown_when_it_is_all_there_is() {
        let stderr = vec!["rsync error: some failure at main.c(1)".to_string()];
        assert!(explain(11, &stderr).contains("some failure"));
    }

    #[test]
    fn an_empty_stderr_still_produces_a_hint() {
        let message = explain(11, &[]);
        assert!(message.contains("exit 11"));
        assert!(message.contains("could not read or write"));
    }

    #[test]
    fn one_connection_serves_many_profiles_unchanged() {
        let connection = connection();
        let mut alpha = profile();
        alpha.remote_path = "/srv/alpha".into();
        let mut beta = profile();
        beta.remote_path = "/srv/beta".into();

        let alpha_args = build_args(&alpha, &connection, &openrsync());
        let beta_args = build_args(&beta, &connection, &openrsync());

        let ssh_of = |args: &[String]| {
            let idx = args.iter().position(|a| a == "-e").unwrap();
            args[idx + 1].clone()
        };
        assert_eq!(ssh_of(&alpha_args), ssh_of(&beta_args));
        assert_eq!(alpha_args.last().unwrap(), "volodia@example.com:'/srv/alpha'");
        assert_eq!(beta_args.last().unwrap(), "volodia@example.com:'/srv/beta'");
    }
}
