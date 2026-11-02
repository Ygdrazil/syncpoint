//! Building and running the rsync command.

mod binary;
mod stream;

pub use binary::{detect, RsyncInfo};

use std::process::Stdio;

use tauri::{AppHandle, Emitter};
use tokio::process::Command;

use crate::connection::Connection;
use crate::profile::Profile;
use stream::{log, parse_progress, read_segments, spawn_log_flusher, StderrLines, SyncProgress};

/// How long rsync goes without any I/O before giving up.
///
/// Without it, a quiet host can be waited on with nothing on the wire at all, so
/// even a dead network goes unnoticed while the run holds one of that SSH
/// master's session permits. This counts silence, not duration, so a transfer
/// that keeps moving can run for hours. A real rsync remote told about the
/// timeout sends keepalives through long quiet phases, so this does not fail a
/// remote that is merely busy. An openrsync remote sends none and enforces the
/// limit itself, so a Mac remote comparing a file of tens of gigabytes can fail
/// here.
///
/// It does not end every stuck run. rsync 3.x counts its own keepalive writes as
/// I/O, so a remote that wedges while the link stays up can outlast it, and on a
/// dead link the run usually ends when ssh gives up rather than at this limit.
const IO_TIMEOUT_SECS: u32 = 300;

/// The same limit under openrsync, which enforces `--timeout` locally but never
/// forwards it. The remote is not told, so it sends no keepalives, and a healthy
/// one walking a large tree says nothing until it is done. Long enough to only
/// ever catch a hang.
const OPENRSYNC_IO_TIMEOUT_SECS: u32 = 3600;

/// The `--timeout` for this binary, if it should get one at all.
///
/// Real rsync before 3.x gets none. Its timer only counts reads, and a sender
/// does not read during a file, so one 60 second write stall would end an upload
/// that had been moving for minutes.
fn io_timeout(rsync: &RsyncInfo) -> Option<u32> {
    if rsync.is_openrsync {
        Some(OPENRSYNC_IO_TIMEOUT_SECS)
    } else if rsync.major >= 3 {
        Some(IO_TIMEOUT_SECS)
    } else {
        None
    }
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
    // rsync stamps mtimes on every directory it walks, which the remote refuses
    // for any directory the SSH user does not own: a tree holding root-created
    // directories fills the log with "failed to set times ... Operation not
    // permitted" and ends at exit 23. Directory mtimes take no part in the
    // skip-unchanged check, so leaving them costs nothing — unlike dropping -t
    // as a whole, which would make every later run re-examine every file.
    // openrsync accepts this despite not listing it in --help.
    args.push("--omit-dir-times".to_string());
    if let Some(timeout) = io_timeout(rsync) {
        args.push(format!("--timeout={timeout}"));
    }

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
        // Drops the "protect" half of every --exclude. Without it a directory
        // that is gone locally can never be removed from the remote while it
        // still holds excluded files, which is not what "delete when deleted
        // locally" says on the tin.
        args.push("--delete-excluded".to_string());
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

    let tx_err = tx.clone();
    let id_err = profile.id.clone();
    let stderr = StderrLines::read(stderr, move |line| {
        log(&tx_err, &id_err, "stderr", line.to_string());
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
    let stderr_lines = stderr.finish().await;

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

/// ssh's own connect failure, if it reported one.
///
/// Matched on ssh's exact prefix rather than by scanning for words like
/// "Permission denied": rsync prints that itself for ordinary per-file
/// failures, so a looser search would relabel an exit 11 or 23 as a login
/// problem. Only a line ssh wrote about reaching the host counts.
fn ssh_connect_failure(stderr: &[String]) -> Option<&String> {
    stderr
        .iter()
        .find(|l| l.trim_start().starts_with("ssh: connect to host"))
}

/// openrsync's report of `--timeout` firing, if it made one.
///
/// rsync 3.x exits 30, but openrsync has no such code: it ends on whatever the
/// transport left behind, and 1, 12, 20 and 247 all turned up in testing. Exit 1
/// would then read as a usage error, so this line is the only reliable sign.
/// Matched on its whole shape, `rsync(<pid>): error: poll: timeout`, so a file
/// named with those words on one line cannot trip it. openrsync prints the same
/// prefix for a per-file error, which then ends in the reason instead.
fn openrsync_timeout(stderr: &[String]) -> Option<&String> {
    stderr.iter().find(|l| {
        let l = l.trim();
        l.starts_with("rsync(") && l.ends_with("): error: poll: timeout")
    })
}

const TIMED_OUT: &str = "Timed out waiting on the remote host.";

fn explain(code: i32, stderr: &[String]) -> String {
    // A host that cannot be reached says so through ssh, whichever exit code
    // rsync settles on afterwards — 255 and 12 both appear for it. That beats
    // the code-based guess, which sent people to debug keys that were fine.
    if let Some(line) = ssh_connect_failure(stderr) {
        return format!(
            "Could not reach the host. It may be down, restarting, or unreachable from \
             this network — the connection never got as far as logging in. (exit {code}) \
             — {}",
            line.trim()
        );
    }

    // The line itself rather than the usual tail, which on openrsync is only the
    // call chain it printed afterwards (io_read_buf, io_read_int).
    if let Some(line) = openrsync_timeout(stderr) {
        return format!("{TIMED_OUT} (exit {code}) — {}", line.trim());
    }

    let hint = match code {
        1 => "rsync usage error — check the excludes and paths.",
        2 => "Protocol mismatch between local and remote rsync.",
        11 => "The remote side could not read or write the destination. Usually the remote \
               directory does not exist, its parent is missing, or the SSH user cannot write \
               there.",
        12 => "Protocol error, usually a failed SSH session or no rsync on the remote host.",
        13 => "Permission denied on the remote side.",
        23 => "Partial transfer — some files could not be sent (permissions?).",
        30 => TIMED_OUT,
        255 => "SSH connection failed. The host answered but the session did not start: \
                with BatchMode on, key auth has to work already. Use Test on this \
                connection in Settings to see ssh's own reason.",
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
    use super::binary::tests::{ancient, modern, openrsync};



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
            folder_id: None,
            position: 0,
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
    fn deleting_carries_excluded_files_with_it() {
        // The two flags are one setting. An --exclude protects the file from
        // deletion on the receiving side as well as hiding it on the sending
        // side, so --delete alone leaves any directory holding one stranded on
        // the remote for good.
        let args = build_args(&profile(), &connection(), &modern());
        assert!(args.contains(&"--delete".to_string()));
        assert!(args.contains(&"--delete-excluded".to_string()));
    }

    #[test]
    fn directory_times_are_left_to_the_remote() {
        // Setting them fails on every directory the SSH user does not own, and
        // that is most of what a root-created remote tree reports. The file
        // times -a brings stay: without those, every later run re-examines
        // every file.
        for rsync in [modern(), openrsync()] {
            let args = build_args(&profile(), &connection(), &rsync);
            assert!(args.contains(&"--omit-dir-times".to_string()));
            assert!(args.contains(&"-a".to_string()), "file times still preserved");
        }
    }

    #[test]
    fn openrsync_tolerates_more_silence_because_it_never_tells_the_remote() {
        // A real rsync remote only sends keepalives when told the timeout, and
        // openrsync does not forward it. The short limit would fail a healthy
        // remote walking a large tree, which is a sync that works without it.
        let timeout_of = |rsync| {
            build_args(&profile(), &connection(), &rsync)
                .into_iter()
                .find(|a| a.starts_with("--timeout="))
        };
        assert_eq!(timeout_of(modern()), Some(format!("--timeout={IO_TIMEOUT_SECS}")));
        assert_eq!(
            timeout_of(openrsync()),
            Some(format!("--timeout={OPENRSYNC_IO_TIMEOUT_SECS}"))
        );
    }

    #[test]
    fn real_rsync_before_3_gets_no_timeout_because_its_timer_only_counts_reads() {
        // A banner that cannot be read lands on major 0, and gets the same.
        let mut unreadable = modern();
        unreadable.major = 0;

        for rsync in [ancient(), unreadable] {
            let args = build_args(&profile(), &connection(), &rsync);
            assert!(!args.iter().any(|a| a.starts_with("--timeout")), "{args:?}");
        }
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
        let program = ssh.split_whitespace().next().unwrap();
        assert!(
            std::path::Path::new(program).is_absolute(),
            "ssh must resolve to an absolute path, not be searched on $PATH: {program}"
        );
        assert!(ssh.contains(" -p 2222 "));
        assert!(ssh.contains("ControlMaster=auto"));
        assert!(ssh.contains("BatchMode=yes"));
        assert!(!ssh.contains('\''), "quotes would be passed through literally");
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
    fn a_host_that_cannot_be_reached_is_not_reported_as_an_auth_problem() {
        // The old 255 text sent people to check their keys. These runs never
        // got as far as logging in, and both exit codes show up for it.
        for code in [255, 12] {
            let stderr = vec![
                "ssh: connect to host dockerdev-421.example port 22: Operation timed out".to_string(),
                "rsync: connection unexpectedly closed (0 bytes received so far) [sender]".to_string(),
                "rsync error: unexplained error (code 255) at io.c(285) [sender=3.5.0]".to_string(),
            ];
            let message = explain(code, &stderr);
            assert!(message.contains("Could not reach the host"), "{message}");
            assert!(message.contains("Operation timed out"), "{message}");
            assert!(!message.contains("key auth"), "still blames the key: {message}");
            assert!(!message.contains("  "), "stray gap in the sentence: {message}");
        }
    }

    #[test]
    fn openrsyncs_call_chain_does_not_push_out_the_reason_a_host_was_unreachable() {
        // The real shape: ssh's line, then openrsync's own trail after it. The
        // last two lines are that trail, which tell nobody anything.
        let stderr = vec![
            "ssh: connect to host h port 22: Connection refused".to_string(),
            "rsync(1): error: unexpected end of file".to_string(),
            "rsync(1): error: io_read_buf".to_string(),
            "rsync(1): error: io_read_int".to_string(),
            "rsync(1): warning: child 2 exited with status 255".to_string(),
        ];
        let message = explain(255, &stderr);
        assert!(message.contains("Could not reach the host"), "{message}");
        assert!(message.contains("Connection refused"), "{message}");
    }

    #[test]
    fn rsyncs_own_permission_errors_are_not_mistaken_for_a_login_failure() {
        // rsync writes "Permission denied" for ordinary per-file failures, so
        // anything matching on that word would relabel a working connection as
        // a broken login. Only a line ssh wrote about reaching the host counts.
        let stderr = vec![
            "rsync: [Receiver] mkdir \"/srv/apps/site\" failed: Permission denied (13)".to_string(),
            "rsync error: error in file IO (code 11) at main.c(791) [Receiver=3.2.7]".to_string(),
        ];
        let message = explain(11, &stderr);
        assert!(!message.contains("Could not reach the host"), "{message}");
        assert!(message.contains("could not read or write"), "{message}");
    }

    #[test]
    fn a_stall_reads_as_a_timeout_on_either_rsync() {
        let stderr = vec![
            "[sender] io timeout after 300 seconds -- exiting".to_string(),
            "rsync error: timeout in data send/receive (code 30) at io.c(248) [sender=3.5.0]".to_string(),
        ];
        let message = explain(30, &stderr);
        assert!(message.contains("Timed out"), "{message}");
        assert!(message.contains("io timeout after 300 seconds"), "{message}");

        // Every code openrsync was seen ending on after a stall. Exit 1 is the
        // one that matters: on its own it would be called a usage error.
        for code in [1, 12, 20, 247] {
            let stderr = vec![
                "rsync(70204): error: poll: timeout".to_string(),
                "rsync(70204): error: io_read_buf".to_string(),
                "rsync(70204): error: io_read_int".to_string(),
            ];
            let message = explain(code, &stderr);
            assert!(message.contains("Timed out"), "{message}");
            assert!(message.contains("poll: timeout"), "{message}");
            assert!(!message.contains("usage error"), "{message}");
        }
    }

    #[test]
    fn a_file_named_like_a_timeout_does_not_read_as_one() {
        for line in [
            "rsync: failed to open \"rsync(1): error: poll: timeout\": Permission denied (13)",
            "rsync: readlink_stat(\"logs\"): error: poll: timeout",
            // openrsync's own per-file shape, which shares the stall's prefix.
            "rsync(70204): error: /src/plain): error: poll: timeout: open (2) in /src: Permission denied",
        ] {
            let message = explain(23, &[line.to_string()]);
            assert!(!message.contains("Timed out"), "{message}");
            assert!(message.contains("Partial transfer"), "{message}");
        }
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
