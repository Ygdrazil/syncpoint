//! The sync engine: filesystem watchers, debouncing and rsync runs.

use std::path::{Path, PathBuf};

use notify::RecommendedWatcher;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::OwnedSemaphorePermit;
use std::time::Duration;

use notify::{EventKind, RecursiveMode, Watcher};
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::time::timeout;

/// Everything needed to keep one profile syncing. Dropping it stops the
/// profile: the watcher shuts down and the sender closes, which ends the loop
/// and tells a run waiting for its turn not to take it.
pub struct ProfileRuntime {
    pub(crate) _watcher: RecommendedWatcher,
    pub(crate) tx: UnboundedSender<Trigger>,
}

use crate::connection::Connection;
use crate::profile::Profile;
use crate::rsync::{self, RsyncInfo};
use crate::state::{mark_synced_unless, set_status, set_status_unless, AppState};

#[derive(Debug, Clone, Copy)]
pub enum Trigger {
    /// The watcher saw something change under the source directory.
    Change,
    /// The user pressed "Sync now".
    Manual,
}

/// What one rsync pass runs on, as it stands at the moment it is looked up.
struct Terms {
    profile: Profile,
    connection: Connection,
    rsync: RsyncInfo,
}

/// `Ok(None)` when the profile no longer exists, which needs no report: whoever
/// deleted it removed its status too.
fn terms_now(state: &AppState, profile_id: &str) -> Result<Option<Terms>, String> {
    let Some(profile) = state.profile(profile_id) else {
        return Ok(None);
    };
    let Some(rsync) = state.rsync_bin.lock().unwrap().clone() else {
        return Err("No rsync binary found on this system.".into());
    };
    let Some(connection) = state.connection(&profile.connection_id) else {
        return Err("This profile's connection no longer exists. Pick a new one.".into());
    };
    Ok(Some(Terms {
        profile,
        connection,
        rsync,
    }))
}

/// Queue behind the other syncs on this SSH master. The slot is held until the
/// run finishes, so only so many share it at once — see MAX_SESSIONS_PER_MASTER.
///
/// A run can wait here for as long as the transfers ahead of it take, and the
/// profile can be stopped, edited or deleted, or its connection repointed, in
/// that time. So everything is decided again when the slot arrives. Running what
/// was true on joining the queue would, with `--delete` on, wipe a remote path
/// the user had since corrected.
///
/// A stopped run keeps its place and gives its turn straight back. `Ok(None)`
/// means the run is no longer wanted.
async fn wait_for_slot(
    state: &AppState,
    profile_id: &str,
    stopped: impl Fn() -> bool,
    on_queued: impl Fn(),
) -> Result<Option<(Terms, OwnedSemaphorePermit)>, String> {
    loop {
        let Some(queued) = terms_now(state, profile_id)? else {
            return Ok(None);
        };
        on_queued();
        // Only fails on a closed semaphore, and nothing closes these.
        let Ok(slot) = state.session_limit(&queued.connection).acquire_owned().await else {
            return Ok(None);
        };

        // Read before checking, in this order. An import stops a profile before
        // changing what it would read, so a run that passes the check is certain
        // to be holding terms from before that import, never ones it brought.
        let terms = terms_now(state, profile_id);
        if stopped() {
            return Ok(None);
        }
        let Some(terms) = terms? else {
            return Ok(None);
        };
        if terms.connection.control_path() == queued.connection.control_path() {
            return Ok(Some((terms, slot)));
        }
        // Repointed at another master while waiting, so this was the wrong
        // queue. The slot goes back before joining the right one: holding both
        // could deadlock against an edit back the other way.
    }
}

/// One rsync pass for a profile, with status transitions around it.
///
/// Once `stopped`, the status is left to whoever stopped the run, in flight or
/// not. See `update_status` for what a refused write still does.
async fn run_once(app: &AppHandle, profile_id: &str, stopped: impl Fn() -> bool) {
    let state = app.state::<AppState>();
    let queued = wait_for_slot(state.inner(), profile_id, &stopped, || {
        // Reported as queued rather than syncing, which is both true and the
        // difference the tray icon counts: thirty profiles at once should not
        // read as thirty transfers when six are running.
        set_status_unless(&stopped, app, profile_id, "pending", None);
    })
    .await;

    let (terms, _slot) = match queued {
        Ok(Some(ready)) => ready,
        Ok(None) => return,
        Err(reason) => {
            set_status_unless(&stopped, app, profile_id, "error", Some(reason));
            return;
        }
    };

    // Refused means stopped since the check in wait_for_slot, so nothing has
    // started. Past this point a stop is one that caught a transfer in flight.
    if !set_status_unless(&stopped, app, profile_id, "syncing", None) {
        return;
    }
    match rsync::run(app, &terms.profile, &terms.connection, &terms.rsync).await {
        Ok(summary) => mark_synced_unless(&stopped, app, profile_id, summary),
        Err(err) => {
            set_status_unless(&stopped, app, profile_id, "error", Some(err));
        }
    }
}

/// One pass for a profile with no loop running, so "Sync now" still works while
/// it is paused. With no channel to close, `asked_at` stands in for one: it is
/// [`AppState::times_stopped`] from when the run was asked for, so anything that
/// stops the profile from then on withdraws the run, as it does for a loop.
pub async fn run_detached(app: &AppHandle, profile_id: &str, asked_at: u64) {
    let state = app.state::<AppState>();
    run_once(app, profile_id, || state.times_stopped(profile_id) != asked_at).await;
}

/// The next trigger, or `None` once the profile is stopped. `recv` alone keeps
/// handing over what was buffered before the channel closed, and those were
/// meant for a profile that has since been switched off.
async fn next_trigger(rx: &mut UnboundedReceiver<Trigger>) -> Option<Trigger> {
    let trigger = rx.recv().await?;
    (!rx.is_closed()).then_some(trigger)
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
        while let Some(trigger) = next_trigger(&mut rx).await {
            if matches!(trigger, Trigger::Change) {
                set_status_unless(|| rx.is_closed(), &app, &profile.id, "pending", None);
                if wait_for_quiet(&mut rx, window).await == Quiet::Stopped {
                    return;
                }
            }
            run_once(&app, &profile.id, || rx.is_closed()).await;
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

/// Start watching a profile, and sync it once to catch up.
pub fn start(app: &AppHandle, profile: &Profile) -> Result<(), String> {
    watch(app, profile)?;

    // Reconcile immediately, and only once the idle status is in place —
    // triggering earlier lets the loop publish "syncing" just before `watch`
    // overwrites it with "idle", hiding a running sync.
    //
    // Without this reconcile at all, edits made while the app was closed would
    // never be sent: the watcher only reports changes from now on, so the
    // remote would sit stale while the UI claimed to be watching.
    trigger(app, &profile.id, Trigger::Manual);
    Ok(())
}

/// Start watching a profile without the first sync, which the caller then owes
/// it. For profiles starting together: see `commands::start_all`.
pub fn watch(app: &AppHandle, profile: &Profile) -> Result<(), String> {
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

    app.state::<AppState>().runtimes.lock().unwrap().insert(
        profile.id.clone(),
        ProfileRuntime {
            _watcher: watcher,
            tx,
        },
    );

    set_status(app, &profile.id, "idle", None);
    Ok(())
}

pub fn stop(app: &AppHandle, profile_id: &str) {
    let state = app.state::<AppState>();
    // Counted and closed before anything is written, here and by the caller: a
    // run checks these under the status lock, so this order is what stops it
    // landing on top of what the profile is now.
    state.note_stopped(profile_id);
    let removed = state.runtimes.lock().unwrap().remove(profile_id);
    if removed.is_some() {
        drop(removed);
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

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use tokio::task::JoinHandle;

    use crate::store::Config;

    type Queued = Result<Option<(Terms, OwnedSemaphorePermit)>, String>;

    fn connection(host: &str) -> Connection {
        Connection {
            id: "c1".into(),
            name: "prod".into(),
            user: "deploy".into(),
            host: host.into(),
            port: 22,
            identity_file: String::new(),
        }
    }

    fn app_state() -> Arc<AppState> {
        let config = Config {
            connections: vec![connection("old.example")],
            profiles: vec![Profile {
                id: "p1".into(),
                name: "site".into(),
                source: "/tmp/src".into(),
                connection_id: "c1".into(),
                remote_path: "/srv/old".into(),
                enabled: true,
                delete: true,
                excludes: Vec::new(),
                debounce_ms: 500,
                folder_id: None,
                position: 0,
            }],
            ..Config::default()
        };
        let state = AppState::new(std::path::PathBuf::from("/tmp"), config);
        *state.rsync_bin.lock().unwrap() = Some(RsyncInfo {
            path: "/usr/bin/rsync".into(),
            version_line: String::new(),
            is_openrsync: true,
            major: 2,
            supports_progress: false,
            protects_args: false,
        });
        Arc::new(state)
    }

    /// Every slot on this host's master taken, as six running transfers would.
    async fn fill(state: &AppState, host: &str) -> OwnedSemaphorePermit {
        let gate = state.session_limit(&connection(host));
        let slots = gate.available_permits() as u32;
        gate.acquire_many_owned(slots).await.unwrap()
    }

    fn free_slots(state: &AppState, host: &str) -> usize {
        state.session_limit(&connection(host)).available_permits()
    }

    fn join_queue(
        state: &Arc<AppState>,
        stopped: impl Fn() -> bool + Send + Sync + 'static,
    ) -> JoinHandle<Queued> {
        let state = state.clone();
        tokio::spawn(async move { wait_for_slot(&state, "p1", stopped, || {}).await })
    }

    /// Long enough for a spawned run to reach the queue.
    async fn settle() {
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    #[tokio::test]
    async fn a_run_stopped_in_the_queue_gives_its_turn_straight_back() {
        // Six transfers ahead, then the user presses Pause: what engine::stop
        // does is drop the sender. The run must not go on to sync.
        let state = app_state();
        let all_slots = free_slots(&state, "old.example");
        let running = fill(&state, "old.example").await;
        let (tx, rx) = mpsc::unbounded_channel::<Trigger>();
        let run = join_queue(&state, move || rx.is_closed());
        settle().await;

        drop(tx);
        drop(running);

        assert!(run.await.unwrap().unwrap().is_none());
        assert_eq!(free_slots(&state, "old.example"), all_slots);
    }

    #[tokio::test]
    async fn any_stop_withdraws_a_one_off_run_from_the_queue() {
        // A one-off has no channel, so this count is all that tells it. Sync now
        // on a paused profile, then Pause, disable, edit, delete or import.
        let state = app_state();
        let running = fill(&state, "old.example").await;
        let asked_at = state.times_stopped("p1");
        let checking = state.clone();
        let run = join_queue(&state, move || checking.times_stopped("p1") != asked_at);
        settle().await;

        state.note_stopped("p1");
        drop(running);

        assert!(run.await.unwrap().unwrap().is_none());
    }

    #[tokio::test]
    async fn a_one_off_run_nothing_stopped_still_runs() {
        let state = app_state();
        let asked_at = state.times_stopped("p1");
        let checking = state.clone();
        let run = join_queue(&state, move || checking.times_stopped("p1") != asked_at);

        assert!(run.await.unwrap().unwrap().is_some());
    }

    #[tokio::test]
    async fn a_run_that_passes_the_stop_check_never_holds_what_an_import_brought() {
        // An import stops a profile, then swaps the config. Should all of that
        // land the instant after this run's check, it must already be holding
        // the old terms: the imported ones were never reviewed.
        let state = app_state();
        let importing = state.clone();
        let stopped = move || {
            importing.profiles.lock().unwrap()[0].remote_path = "/imported".into();
            false
        };

        let (terms, _slot) = wait_for_slot(&state, "p1", stopped, || {}).await.unwrap().unwrap();

        assert_eq!(terms.profile.remote_path, "/srv/old");
    }

    #[tokio::test]
    async fn a_run_queued_before_an_edit_runs_on_the_edit() {
        // With --delete on, the old remote path is the one the user corrected.
        let state = app_state();
        let running = fill(&state, "old.example").await;
        let run = join_queue(&state, || false);
        settle().await;

        state.profiles.lock().unwrap()[0].remote_path = "/srv/new".into();
        drop(running);

        let (terms, _slot) = run.await.unwrap().unwrap().unwrap();
        assert_eq!(terms.profile.remote_path, "/srv/new");
    }

    #[tokio::test]
    async fn a_repointed_connection_moves_the_run_to_the_right_queue() {
        let state = app_state();
        let all_slots = free_slots(&state, "old.example");
        let old_running = fill(&state, "old.example").await;
        let run = join_queue(&state, || false);
        settle().await;

        state.connections.lock().unwrap()[0].host = "new.example".into();
        let new_running = fill(&state, "new.example").await;
        drop(old_running);
        settle().await;

        // The new host's limit applies to it now, and it holds no slot on the
        // old one while it waits.
        assert!(!run.is_finished(), "must queue behind the new host's transfers");
        assert_eq!(free_slots(&state, "old.example"), all_slots);

        drop(new_running);
        let (terms, _slot) = run.await.unwrap().unwrap().unwrap();
        assert_eq!(terms.connection.host, "new.example");
        assert_eq!(free_slots(&state, "new.example"), all_slots - 1);
    }

    #[tokio::test]
    async fn an_edit_that_keeps_the_master_keeps_its_place() {
        let state = app_state();
        let running = fill(&state, "old.example").await;
        let times_queued = Arc::new(AtomicUsize::new(0));
        let run = {
            let (state, times_queued) = (state.clone(), times_queued.clone());
            tokio::spawn(async move {
                let joined = || {
                    times_queued.fetch_add(1, Ordering::SeqCst);
                };
                wait_for_slot(&state, "p1", || false, joined).await
            })
        };
        settle().await;

        state.connections.lock().unwrap()[0].name = "renamed".into();
        drop(running);

        let (terms, _slot) = run.await.unwrap().unwrap().unwrap();
        assert_eq!(terms.connection.name, "renamed");
        assert_eq!(times_queued.load(Ordering::SeqCst), 1, "sent to the back of the queue");
    }

    #[tokio::test]
    async fn a_profile_deleted_in_the_queue_is_dropped_without_a_report() {
        let state = app_state();
        let running = fill(&state, "old.example").await;
        let run = join_queue(&state, || false);
        settle().await;

        state.profiles.lock().unwrap().clear();
        drop(running);

        assert!(run.await.unwrap().unwrap().is_none());
    }

    #[tokio::test]
    async fn a_connection_deleted_in_the_queue_is_reported_instead_of_used() {
        let state = app_state();
        let running = fill(&state, "old.example").await;
        let run = join_queue(&state, || false);
        settle().await;

        state.connections.lock().unwrap().clear();
        drop(running);

        let reason = run.await.unwrap().err().unwrap();
        assert!(reason.contains("no longer exists"), "{reason}");
    }

    #[tokio::test]
    async fn triggers_buffered_before_a_stop_are_not_acted_on() {
        // A closed channel still hands these over, and acting on them would
        // sync a profile the user has just switched off.
        let (tx, mut rx) = mpsc::unbounded_channel::<Trigger>();
        tx.send(Trigger::Manual).unwrap();
        drop(tx);

        assert!(next_trigger(&mut rx).await.is_none());
    }

    #[tokio::test]
    async fn triggers_on_a_running_profile_still_arrive() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Trigger>();
        tx.send(Trigger::Manual).unwrap();

        assert!(next_trigger(&mut rx).await.is_some());
    }

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
