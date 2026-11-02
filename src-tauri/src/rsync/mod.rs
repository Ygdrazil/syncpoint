//! Building and running the rsync command.

mod binary;
mod stream;

pub use binary::{detect, RsyncInfo};

use std::process::Stdio;

use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::connection::Connection;
use crate::profile::Profile;
use stream::{log, parse_progress, read_segments, spawn_log_flusher, SyncProgress};

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

fn explain(code: i32, stderr: &[String]) -> String {
    // A host that cannot be reached says so through ssh, whichever exit code
    // rsync settles on afterwards — 255 and 12 both appear for it. That beats
    // the code-based guess, which sent people to debug keys that were fine.
    if ssh_connect_failure(stderr).is_some() {
        let detail = most_useful_lines(stderr)
            .into_iter()
            .map(|l| l.trim())
            .collect::<Vec<_>>()
            .join(" / ");
        return format!(
            "Could not reach the host. It may be down, restarting, or unreachable from              this network — the connection never got as far as logging in. (exit {code})              — {detail}"
        );
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
        30 => "Timed out waiting on the remote host.",
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
    use super::binary::tests::{modern, openrsync};



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
        }
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
