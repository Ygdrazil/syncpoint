//! Turning rsync's output into events the frontend can render.
//!
//! Two jobs: batching the log so a large sync cannot flood the webview, and
//! picking the progress line out of ordinary output.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncRead, AsyncReadExt, BufReader};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::time::{timeout, MissedTickBehavior};

/// How often batched output is pushed to the frontend.
const FLUSH_INTERVAL_MS: u64 = 100;
/// Ceiling on lines carried by one batch. `rsync -v` prints a line per file, so
/// a first sync of a large tree produces tens of thousands of them. The UI keeps
/// only a bounded scrollback, so shipping more than this per flush is wasted
/// work on both sides of the IPC boundary — and enough of it freezes the
/// webview's main thread. Dropped lines are counted, never silently lost: the
/// complete transcript still goes to stderr.
const MAX_BATCH_LINES: usize = 400;
/// How long to keep reading stderr once rsync has exited.
///
/// EOF can be a long way off: an ssh client hands its stderr to the SSH master,
/// which keeps it until the remote closes the session. With a stuck remote that
/// may be never, and waiting would leave a finished run showing as syncing,
/// holding its session permit, with its error unreported. What rsync wrote
/// itself is already in the pipe, so this only has to be long enough to read it.
const STDERR_DRAIN: Duration = Duration::from_secs(2);

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LogLine {
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
pub(super) fn spawn_log_flusher(app: AppHandle, profile_id: String) -> UnboundedSender<LogLine> {
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
pub(super) struct SyncProgress {
    pub(super) profile_id: String,
    pub(super) percent: u8,
    pub(super) transferred: String,
    pub(super) rate: String,
    pub(super) eta: String,
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
pub(super) fn parse_progress(line: &str) -> Option<(u8, String, String, String)> {
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
pub(super) async fn read_segments<R: AsyncRead + Unpin>(reader: R, mut emit: impl FnMut(String)) {
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

/// rsync's stderr, collected as well as streamed, so a failure can report the
/// actual reason rather than just an exit code.
pub(super) struct StderrLines {
    lines: Arc<Mutex<Vec<String>>>,
    reader: tauri::async_runtime::JoinHandle<()>,
}

impl StderrLines {
    pub(super) fn read<R>(stderr: R, mut on_line: impl FnMut(&str) + Send + 'static) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        let reader = tauri::async_runtime::spawn(async move {
            read_segments(stderr, |line| {
                on_line(&line);
                sink.lock().unwrap().push(line);
            })
            .await;
        });
        Self { lines, reader }
    }

    /// Everything read by the time stderr closes or [`STDERR_DRAIN`] runs out.
    /// Call once rsync has exited.
    pub(super) async fn finish(mut self) -> Vec<String> {
        if timeout(STDERR_DRAIN, &mut self.reader).await.is_err() {
            self.reader.abort();
        }
        std::mem::take(&mut *self.lines.lock().unwrap())
    }
}

pub(super) fn log(tx: &UnboundedSender<LogLine>, profile_id: &str, stream: &'static str, line: String) {
    // Mirrored to stderr as well as the UI, so a full transcript can be
    // captured to a file for bug reports. See "Getting logs" in the README.
    eprintln!("[syncpoint {profile_id} {stream}] {line}");
    let _ = tx.send(LogLine { line, stream });
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::process::Stdio;
    use std::time::Instant;
    use tokio::process::{Child, Command};

    fn shell(script: &str) -> Child {
        Command::new("/bin/sh")
            .args(["-c", script])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    #[tokio::test]
    async fn a_stderr_held_open_cannot_keep_a_finished_run_waiting() {
        // `sleep` inherits stderr and outlives the shell, the way an SSH master
        // outlives the rsync whose ssh handed it that stderr.
        let mut child = shell("echo boom >&2; sleep 8 & exit 30");
        let stderr = StderrLines::read(child.stderr.take().unwrap(), |_| {});
        assert_eq!(child.wait().await.unwrap().code(), Some(30));

        let started = Instant::now();
        let lines = stderr.finish().await;

        assert_eq!(lines, ["boom"]);
        assert!(started.elapsed() < Duration::from_secs(6), "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn a_stderr_that_closes_is_not_waited_on_at_all() {
        let mut child = shell("echo boom >&2");
        let stderr = StderrLines::read(child.stderr.take().unwrap(), |_| {});
        child.wait().await.unwrap();

        let started = Instant::now();
        assert_eq!(stderr.finish().await, ["boom"]);
        assert!(started.elapsed() < STDERR_DRAIN, "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn a_line_that_is_not_utf8_does_not_end_collection() {
        // A remote shell profile printing Latin-1 reaches here through ssh. If
        // that ended the read, rsync's next error would find the pipe closed and
        // end the run, and the line saying what went wrong would never arrive.
        let mut child = shell(r"printf 'first\n\351\nlast\n' >&2");
        let stderr = StderrLines::read(child.stderr.take().unwrap(), |_| {});
        assert_eq!(child.wait().await.unwrap().code(), Some(0));

        let lines = stderr.finish().await;

        assert_eq!(lines.first().map(String::as_str), Some("first"));
        assert_eq!(lines.last().map(String::as_str), Some("last"));
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
}
