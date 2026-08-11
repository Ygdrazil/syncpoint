//! The sync engine: filesystem watchers, debouncing and rsync runs.

use std::path::{Path, PathBuf};

use notify::RecommendedWatcher;
use tokio::sync::mpsc::UnboundedSender;
use std::time::Duration;

use notify::{EventKind, RecursiveMode, Watcher};
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::time::timeout;

/// Everything needed to keep one profile syncing. Dropping it stops the
/// profile: the watcher shuts down and the sender closes, which ends the loop.
pub struct ProfileRuntime {
    pub(crate) _watcher: RecommendedWatcher,
    pub(crate) tx: UnboundedSender<Trigger>,
}

use crate::profile::Profile;
use crate::rsync;
use crate::state::{mark_synced, set_status, AppState};

#[derive(Debug, Clone, Copy)]
pub enum Trigger {
    /// The watcher saw something change under the source directory.
    Change,
    /// The user pressed "Sync now".
    Manual,
}

/// One rsync pass for a profile, with status transitions around it.
///
/// The connection is resolved per run rather than captured when the watcher
/// starts, so editing a connection takes effect on the next sync without
/// having to restart every profile that uses it.
pub async fn run_once(app: &AppHandle, profile: &Profile) {
    let (rsync, connection) = {
        let state = app.state::<AppState>();
        let rsync = state.rsync_bin.lock().unwrap().clone();
        (rsync, state.connection(&profile.connection_id))
    };

    let Some(rsync) = rsync else {
        set_status(
            app,
            &profile.id,
            "error",
            Some("No rsync binary found on this system.".into()),
        );
        return;
    };
    let Some(connection) = connection else {
        set_status(
            app,
            &profile.id,
            "error",
            Some("This profile's connection no longer exists. Pick a new one.".into()),
        );
        return;
    };

    set_status(app, &profile.id, "syncing", None);
    match rsync::run(app, profile, &connection, &rsync).await {
        Ok(summary) => mark_synced(app, &profile.id, summary),
        Err(err) => set_status(app, &profile.id, "error", Some(err)),
    }
}

#[derive(Debug, PartialEq)]
enum Quiet {
    /// No events for a full debounce window; safe to sync.
    Settled,
    /// The profile was stopped while waiting.
    Stopped,
}

/// Swallow triggers until the source has been quiet for `window`. Each new
/// event restarts the timer, so a burst of writes (a save storm, a build
/// writing an output directory) collapses into a single rsync run.
async fn wait_for_quiet(rx: &mut UnboundedReceiver<Trigger>, window: Duration) -> Quiet {
    loop {
        match timeout(window, rx.recv()).await {
            Ok(Some(_)) => continue,       // more activity, restart the timer
            Ok(None) => return Quiet::Stopped,
            Err(_) => return Quiet::Settled,
        }
    }
}

/// The per-profile sync loop.
///
/// Triggers arrive on an unbounded channel. Filesystem changes are debounced;
/// a manual sync runs at once. Anything that lands *while* rsync is running
/// stays queued, so the loop immediately does another pass and the remote never
/// sits stale.
fn spawn_sync_loop(app: AppHandle, profile: Profile, mut rx: UnboundedReceiver<Trigger>) {
    tauri::async_runtime::spawn(async move {
        let window = Duration::from_millis(profile.debounce_ms);
        while let Some(trigger) = rx.recv().await {
            if matches!(trigger, Trigger::Change) {
                set_status(&app, &profile.id, "pending", None);
                if wait_for_quiet(&mut rx, window).await == Quiet::Stopped {
                    return;
                }
            }
            run_once(&app, &profile).await;
        }
    });
}

/// Approximate check used only to avoid waking the sync loop for paths rsync
/// would skip anyway. rsync's own `--exclude` remains authoritative for what
/// actually gets transferred; a false negative here just costs one extra run.
fn is_excluded(path: &Path, source: &Path, excludes: &[String]) -> bool {
    let Ok(relative) = path.strip_prefix(source) else {
        return false;
    };
    let relative_str = relative.to_string_lossy();

    excludes.iter().any(|pattern| {
        let pattern = pattern.trim().trim_end_matches('/');
        if pattern.is_empty() {
            return false;
        }
        if pattern.contains('/') {
            relative_str.starts_with(pattern.trim_start_matches('/'))
        } else {
            relative
                .components()
                .any(|c| glob_match(pattern, &c.as_os_str().to_string_lossy()))
        }
    })
}

/// Leading/trailing `*` only — enough for the patterns people actually type
/// (`.git`, `*.log`, `node_modules`, `.DS_Store`).
fn glob_match(pattern: &str, name: &str) -> bool {
    let starts = pattern.starts_with('*');
    let ends = pattern.ends_with('*');
    let core = pattern.trim_matches('*');
    if core.is_empty() {
        return true;
    }
    match (starts, ends) {
        (true, true) => name.contains(core),
        (true, false) => name.ends_with(core),
        (false, true) => name.starts_with(core),
        (false, false) => name == core,
    }
}

pub fn start(app: &AppHandle, profile: &Profile) -> Result<(), String> {
    stop(app, &profile.id);
    profile.validate()?;
    if app.state::<AppState>().connection(&profile.connection_id).is_none() {
        return Err("This profile's connection no longer exists. Pick a new one.".into());
    }

    let (tx, rx) = mpsc::unbounded_channel::<Trigger>();

    let source = PathBuf::from(&profile.source);
    let excludes = profile.excludes.clone();
    let watcher_source = source.clone();
    let watcher_tx = tx.clone();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        // Reads and metadata-only opens do not change content worth sending.
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        let relevant = event
            .paths
            .iter()
            .any(|p| !is_excluded(p, &watcher_source, &excludes));
        if relevant {
            let _ = watcher_tx.send(Trigger::Change);
        }
    })
    .map_err(|e| format!("Could not create watcher: {e}"))?;

    watcher
        .watch(&source, RecursiveMode::Recursive)
        .map_err(|e| format!("Could not watch {}: {e}", profile.source))?;

    spawn_sync_loop(app.clone(), profile.clone(), rx);
    let initial = tx.clone();

    app.state::<AppState>().runtimes.lock().unwrap().insert(
        profile.id.clone(),
        ProfileRuntime {
            _watcher: watcher,
            tx,
        },
    );

    set_status(app, &profile.id, "idle", None);

    // Reconcile immediately, and only once the idle status is in place —
    // triggering earlier lets the loop publish "syncing" just before this
    // function overwrites it with "idle", hiding a running sync.
    //
    // Without this reconcile at all, edits made while the app was closed would
    // never be sent: the watcher only reports changes from now on, so the
    // remote would sit stale while the UI claimed to be watching.
    let _ = initial.send(Trigger::Manual);
    Ok(())
}

pub fn stop(app: &AppHandle, profile_id: &str) {
    let removed = app
        .state::<AppState>()
        .runtimes
        .lock()
        .unwrap()
        .remove(profile_id);
    if removed.is_some() {
        set_status(app, profile_id, "disabled", None);
    }
}

/// Queue a manual run on a profile's live loop. Returns false when the profile
/// is not running, so the caller can fall back to a one-off pass.
pub fn trigger(app: &AppHandle, profile_id: &str, trigger: Trigger) -> bool {
    let state = app.state::<AppState>();
    let runtimes = state.runtimes.lock().unwrap();
    match runtimes.get(profile_id) {
        Some(runtime) => runtime.tx.send(trigger).is_ok(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_handles_the_common_patterns() {
        assert!(glob_match(".git", ".git"));
        assert!(!glob_match(".git", ".github"));
        assert!(glob_match("*.log", "server.log"));
        assert!(!glob_match("*.log", "log.txt"));
        assert!(glob_match("build*", "build_output"));
        assert!(glob_match("*cache*", "my_cache_dir"));
        assert!(glob_match("*", "anything"));
    }

    #[test]
    fn excludes_match_any_path_component() {
        let source = Path::new("/tmp/src");
        let excludes = vec![".git".to_string(), "*.log".to_string()];

        assert!(is_excluded(Path::new("/tmp/src/.git/HEAD"), source, &excludes));
        assert!(is_excluded(Path::new("/tmp/src/a/b/x.log"), source, &excludes));
        assert!(!is_excluded(Path::new("/tmp/src/a/main.rs"), source, &excludes));
    }

    #[test]
    fn excludes_with_a_slash_anchor_to_the_source_root() {
        let source = Path::new("/tmp/src");
        let excludes = vec!["target/debug".to_string()];

        assert!(is_excluded(
            Path::new("/tmp/src/target/debug/app"),
            source,
            &excludes
        ));
        // Same name deeper in the tree is not anchored, so it is not excluded.
        assert!(!is_excluded(
            Path::new("/tmp/src/sub/target/debug/app"),
            source,
            &excludes
        ));
    }

    #[tokio::test]
    async fn a_burst_of_events_collapses_into_one_sync() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Trigger>();
        for _ in 0..50 {
            tx.send(Trigger::Change).unwrap();
        }

        let started = std::time::Instant::now();
        assert_eq!(
            wait_for_quiet(&mut rx, Duration::from_millis(40)).await,
            Quiet::Settled
        );
        // All 50 events were consumed by the single wait, so the caller runs
        // rsync once rather than 50 times.
        assert!(rx.try_recv().is_err());
        assert!(started.elapsed() >= Duration::from_millis(40));
    }

    #[tokio::test]
    async fn each_new_event_restarts_the_quiet_period() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Trigger>();
        // `tx` is held by the test for the whole wait, so the channel stays
        // open and the wait can only end by timing out.
        let sender = tx.clone();
        tokio::spawn(async move {
            // Four events spaced inside the window, then silence.
            for _ in 0..4 {
                tokio::time::sleep(Duration::from_millis(25)).await;
                if sender.send(Trigger::Change).is_err() {
                    return;
                }
            }
        });

        let started = std::time::Instant::now();
        assert_eq!(
            wait_for_quiet(&mut rx, Duration::from_millis(50)).await,
            Quiet::Settled
        );
        // It must not have settled after the first 50ms: the trailing events
        // pushed the deadline out past 100ms.
        assert!(started.elapsed() >= Duration::from_millis(100));
    }

    #[tokio::test]
    async fn stopping_the_profile_ends_the_wait() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Trigger>();
        tx.send(Trigger::Change).unwrap();
        drop(tx);
        assert_eq!(
            wait_for_quiet(&mut rx, Duration::from_secs(30)).await,
            Quiet::Stopped
        );
    }

    #[test]
    fn paths_outside_the_source_are_never_excluded() {
        let source = Path::new("/tmp/src");
        assert!(!is_excluded(
            Path::new("/elsewhere/.git/HEAD"),
            source,
            &[".git".to_string()]
        ));
    }
}
