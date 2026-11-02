# AGENT.md

This file provides guidance to AI coding agents (Claude Code and others that read this file) when working with code in this repository.

## What this is

SyncPoint: a macOS menu-bar app that mirrors a local folder to a remote host over SSH. A Rust/Tauri 2 backend watches the filesystem and shells out to `rsync`; a SvelteKit/Svelte 5 frontend (compiled to static assets, no SSR) renders every window. One pnpm project, one Cargo crate — not a monorepo, so this single file covers both halves.

## Commands

```bash
pnpm install                # once, and after pulling dependency changes

pnpm tauri dev               # run the full app (spawns the Vite dev server itself)

cargo test --lib --manifest-path src-tauri/Cargo.toml            # all Rust tests
cargo test --lib --manifest-path src-tauri/Cargo.toml folder::    # one module, e.g. folder.rs
cargo test --lib --manifest-path src-tauri/Cargo.toml a_leading_dash_is_refused  # one test, by name substring

pnpm check                   # svelte-kit sync + svelte-check (TypeScript + Svelte types)

pnpm tauri build                                    # release build, host arch only
pnpm tauri build --target universal-apple-darwin    # what release.yml actually ships
```

There is no frontend unit-test runner (no vitest/jest configured) — `pnpm check` (strict TS + svelte-check) is the only frontend gate. CI (`.github/workflows/ci.yml`) runs exactly `cargo test --lib --manifest-path src-tauri/Cargo.toml` and `pnpm check`, on **Ubuntu**, not macOS.

That last point matters: `src-tauri/src/menu.rs` has `#[cfg(target_os = "macos")]` / `#[cfg(not(target_os = "macos"))]` branches for the app menu. CI only ever compiles the `not(macos)` arm. The macOS arm — the one that actually ships — is not compiled or tested by CI. Verify menu changes on a real Mac (`pnpm tauri dev`), don't trust a green CI run for anything behind that cfg.

## Architecture

### One SPA, three windows

Every window (`main`, `settings`, `about`) loads the same route; `src/routes/+page.svelte` picks the view by `getCurrentWindow().label`. There is no client-side router and no SSR (`ssr = false` in `+layout.ts`, `adapter-static` with an `index.html` fallback) — this is required by Tauri, which has no Node server to render against. Auxiliary windows are opened/focused through `menu::focus_or_create` (`src-tauri/src/menu.rs`), driven by a `WindowSpec` struct rather than one-off `WebviewWindowBuilder` calls per window.

Because each window is a separate webview (separate JS realm), there is no shared in-memory frontend state. Windows stay in sync purely through Tauri's event bus: `config-changed` (any backend mutation), `sync-status`, `sync-progress`, `sync-log`, `settings-navigate`, `new-profile-from`. If you add state that multiple windows must agree on, it belongs in `AppState` on the backend, not in a Svelte store.

**Stash-and-take pattern**: opening a window with a payload it should act on (which settings category to land on, whether About should auto-start a download) is done by stashing a value in `AppState` (`pending_settings_section`, `pending_auto_update`) before the window exists, and having the window `take_*` it once on mount. A window that's already open is told via an event instead — stashing for that case would leave a stale value for the *next* open. Follow this pattern if you add a fourth window with the same need.

### Backend module map (`src-tauri/src/`)

`AppState` (`state.rs`) is the single source of truth: `Mutex`-wrapped `Settings` / `Vec<Connection>` / `Vec<Folder>` / `Vec<Profile>` / status map, plus one `tokio::sync::Semaphore` per SSH master. Every mutating `#[tauri::command]` in `commands.rs` follows the same shape: lock, mutate a `Config` snapshot, `state.replace_collections(...)`, `state.persist()`, then `app.emit("config-changed", ())`. `store.rs` persists that `Config` to `config.json` in the OS app-config dir via write-to-temp-then-rename (crash-safe), and transparently migrates a pre-connections `profiles.json` on first load.

| Module | Owns |
|---|---|
| `engine.rs` | Per-profile watcher lifecycle, debounce loop, one rsync run (`run_once`) |
| `rsync/mod.rs` | argv construction, spawning, exit-code → human-readable error |
| `rsync/binary.rs` | Finds the rsync binary, detects its capabilities from `--version`/`--help` |
| `rsync/stream.rs` | Batches stdout/stderr into throttled UI events, parses `--info=progress2` lines |
| `connection.rs` | SSH connection record + injection-safe arg building + live `ssh` test |
| `profile.rs` | Sync profile record + destination-safety validation |
| `folder.rs` | The folder tree profiles live in (folders and profiles share one ordering per level) |
| `transfer.rs` | Export/import bundles (merge-by-id, re-validates untrusted input) |
| `tray.rs` / `menu.rs` | Tray icon, app menu, window open/focus, quit-vs-hide |

### Sync engine

One `notify` watcher + one unbounded `mpsc` channel per enabled profile (`engine::start`). A filesystem event sets status to `pending` and restarts a debounce timer (`profile.debounce_ms`, 50–60000ms, enforced in both `profile.rs` and `settings.rs`); anything that lands while a sync is already running is picked up by the next loop iteration rather than being dropped. Each run acquires a permit from `AppState::session_limit(connection)` before syncing — one gate per SSH master rather than per connection record, since two records with the same login share a master — capped at `MAX_SESSIONS_PER_MASTER` (`state.rs`, currently 6) because sshd's default `MaxSessions` is 10 and going near that makes concurrent syncs to one host fail outright. A run can wait a long time for that permit, so `engine::wait_for_slot` looks up the profile and connection again once it arrives: a stopped run gives its turn straight back, and one whose connection moved to another master requeues there. "Stopped" means `engine::stop` ran for that profile, whatever the reason: a loop run sees its channel closed, and a one-off (`engine::run_detached`, which has no channel) sees `AppState::times_stopped` change, so a queued "Sync now" is withdrawn by disabling, editing or deleting that profile, by an import that changes its terms, or by Pause all if the profile was enabled. Pause all leaves alone a profile already paused on its own, so a "Sync now" queued on one still runs. Anything a run writes to the status goes through the `_unless` setters in `state.rs`, which check `stopped` under the status lock, and `engine::stop` counts and closes before anything is written; keep that order, or a run can land on top of the status its stopper just set. `import_config` stops profiles before swapping the config in, and `wait_for_slot` reads terms before checking `stopped`; together those are what keep an imported, unreviewed destination away from a run already queued. SSH itself is reused across runs via `ControlMaster`/`ControlPersist` (`connection.rs`), so only the first sync per login (user, host, port, key) pays the handshake. Profiles starting together (launch, Resume, Sync all) go through `commands::sync_together`, which opens each master once before they start; otherwise they race to create it, and every loser logs `ControlSocket ... already exists` and dials its own connection.

`rsync/binary.rs::detect()` probes for a real binary and classifies it (Apple's ancient bundled `openrsync` vs. modern rsync 3.x), and `rsync/mod.rs::build_args` conditions flags on that: `--info=progress2`/`--outbuf=L` only if the binary supports it, `--old-args` only on rsync 3.2.4+ (needed because that version started escaping remote args itself, which double-escapes this app's own quoting otherwise), `--timeout` by binary (`rsync/mod.rs::io_timeout`, none at all on real rsync before 3.x). Don't add a flag to `build_args` without checking whether openrsync (protocol 29, no `--info`, no long-option parity with GNU rsync) can survive it.

### Destination-safety invariants

`Connection::check_safe()` (leading-`-` and whitespace rejected in host/user/identity-file — otherwise `ssh` reads a hostile value as a flag, e.g. `-oProxyCommand=...`) is deliberately separate from `Connection::validate()` (machine-local checks, e.g. does this key file exist *here*). `transfer::import` calls `check_safe()` on every incoming connection but not the full `validate()`, because an imported bundle may legitimately name a key path from a different Mac — that's a reason it can't run yet, not a reason to reject the import. Keep that split if you touch either path.

`profile.rs::PROTECTED_REMOTE_PATHS` blocks syncing (with `--delete` on by default) straight onto `/`, `/etc`, `/usr`, `/home`, `~`, etc., and `.`/`..` path segments are rejected outright since they resolve on the remote shell, past this app's own checks. If you add a new way to set a profile's remote path (bulk edit, template, whatever), route it through `Profile::validate()`, don't reimplement the checks.

### Frontend

Svelte 5 runes only (`$state`/`$derived`/`$effect`), no external state library. `src/lib/ProfilesView.svelte` is the main window and by far the largest file — it implements its own pointer-based drag-and-drop for the folder/profile tree. This is not incidental complexity: native HTML5 drag-and-drop does not work in this webview because wry intercepts `draggingEntered`/`performDragOperation` on the WKWebView itself before WebKit's DOM drag events fire. Don't try to replace it with `draggable="true"` — it's been tried and doesn't work here.

Native `<dialog>` is used for confirm/prompt dialogs (`ConfirmDialog.svelte`, `PromptDialog.svelte`) instead of `confirm()`/`prompt()`, because WKWebView's `WKUIDelegate` never implements `runJavaScriptTextInputPanel`, so `prompt()` silently returns `null`.

Theming (`src/styles/_themes.scss`) is a single Sass map (`$themes`) keyed by the same strings as the Rust `Theme` enum (`settings.rs`) — a Rust test (`settings::tests::every_explicit_theme_has_a_palette`) reads that SCSS file at test time and fails if a theme variant has no matching palette entry. Add a theme in both places or the test catches it.

### Release & auto-update

`src-tauri/build.rs` bakes the running version into the binary as `SYNCPOINT_VERSION`, from `git describe --tags --always` — this is what `version::describe()` shows in the About window. Separately, `tauri.conf.json`'s `version` field (which feeds the macOS bundle's `Info.plist`) is bumped by `release.yml` itself at build time, from the pushed tag. These are two different mechanisms serving two different consumers; don't assume bumping one updates the other.

`release.yml` only runs on a pushed `vX.Y.Z` tag, builds a universal macOS binary, and produces both a DMG (manual download) and `.app.tar.gz` + `.sig` (what the Tauri updater actually fetches, per the generated `latest.json`). Updater authenticity is a separate minisign keypair from Apple code signing — the pubkey lives in `tauri.conf.json`, the private key is the `TAURI_SIGNING_PRIVATE_KEY` GitHub secret. Apple signing is ad-hoc (`signingIdentity: "-"`), so this pipeline needs no Apple Developer account.

### Code style note

Comments in this codebase explain *why*, essentially never *what* — the existing Rust and Svelte files are the reference for the expected density and tone. Match it rather than adding narrative comments above self-explanatory code.

## Framework practices

Orientation for the three technologies this repo combines. These are conventions already established in the code — follow them rather than defaulting to generic (or React/Svelte-4/anyhow-style) idioms that don't match what's here.

### Tauri

- **Main-thread-only APIs.** Tray and menu handles (`AppHandle::tray_by_id`, and anything on the returned `TrayIcon`) must never be created *or dropped* off the main thread. `tray::set_syncing` calls originate from tokio tasks (the sync loop) as often as from the main thread (Tauri commands), so it wraps its entire body in `app.run_on_main_thread(move || { ... })` — the clone-and-drop of the tray handle has to happen on the same thread it was created on, or macOS crashes with `EXC_BREAKPOINT` in `NSStatusBar._removeStatusItem:`. This already happened once (see `.serena/memories/tauri-tray-main-thread-crash.md`). Any new code that touches `tray_by_id` from a context that might run on a tokio worker needs the same wrapper — don't assume "it's just reading an icon" is safe.
- **Window lifecycle is not uniform.** Only the `main` window intercepts `CloseRequested` and hides instead of closing (`lib.rs`'s `on_window_event`, gated on `window.label() == "main"`). `settings` and `about` close for real — their Svelte components fully unmount, and `menu::focus_or_create` builds them from scratch next time (`get_webview_window` returns `None` after a real close). Don't add per-window state to those two assuming it survives a close; anything that must survive belongs in `AppState` (see the stash-and-take pattern above).
- **Keep `#[tauri::command]` wrappers thin.** The convention throughout `commands.rs` is: the command function validates input, touches `AppState`, and delegates the actual logic to a plain function in the relevant module (`folder::remove`, `transfer::import`, `connection::test`, …). Write and unit-test *that* function directly — there's no test harness here that spins up a Tauri app, and there doesn't need to be.
- **Capabilities stay minimal.** `capabilities/default.json` grants `core:default`, `dialog:default`, `updater:default`, `process:default`, and two specific `core:window` permissions — no `fs:default`, `shell:default`, or `http:default`. Every actual filesystem/process/network action goes through a narrow custom command instead. If a new feature seems to need a broad plugin permission, that's usually a sign the logic belongs in a new Rust command, not in the webview.

### Svelte 5

- **Runes, not legacy reactivity.** Every component in this codebase uses `$state`, `$derived` / `$derived.by`, `$effect`, and `$props()`. There is no `export let`, no top-level `$:`, and no `writable`/`readable` store anywhere — cross-*window* sync goes through Tauri events (see above), and within a component tree, runes are enough. Don't introduce the Svelte-4 patterns or the store API.
- **Props pattern.** Declare a `type Props = { ... }` and destructure it: `let { title, onconfirm, oncancel }: Props = $props()` (see `ConfirmDialog.svelte`, `PromptDialog.svelte`, `Toasts.svelte`, `Hint.svelte`). Callback props (`onconfirm`, `ondismiss`, …) are plain functions passed down, not `createEventDispatcher` — that API isn't used here.
- **Snippets replace slots.** `Hint.svelte` takes `children: Snippet` and renders it with `{@render children()}`. Use `{#snippet ...}` / `{@render ...}` for anything that would have been a named slot in Svelte 4.
- **`$derived` for values, `$effect` for side effects — don't blur the two.** `rows`, `selected`, `compact`, `paused` in `ProfilesView.svelte` are all `$derived`/`$derived.by`: pure computations from other state. `$effect` is reserved for genuine side effects with no return value: pushing `compact` to the backend, calling `showModal()`/`.select()` on a dialog, auto-scrolling the log pane. If you're computing a value, it's `$derived`; if you're *doing* something in response to a value changing, it's `$effect`.
- **In-place mutation of `$state` is intentional here, not a bug to fix.** `toasts.push(...)`, `statuses[id] = ...`, `logs[profileId].push(entry)` all mutate `$state` containers directly rather than reassigning a spread copy. This is deliberate — the sync-log listener has an explicit comment that rebuilding the array per line is what froze the UI during a large first sync. Svelte 5's `$state` proxies track these mutations; don't "clean it up" into `arr = [...arr, x]`.

### Rust

- **Command boundary errors are plain `String`, not `anyhow`/`thiserror`.** Every fallible command returns `Result<T, String>`, and the frontend shows that string directly to the user (`formError = String(err)`, toast text, etc.) — so error messages are written as complete, specific sentences ("Refusing to sync onto /etc: …", not "invalid path"). Keep new errors in that voice; don't introduce a structured error enum at this boundary.
- **Don't hold a `std::sync::Mutex` guard across an `.await`.** `AppState`'s fields use `std::sync::Mutex` (not tokio's), because every critical section is short and non-blocking. The pattern everywhere in `commands.rs` and `engine.rs` is: lock inside a `{ }` block, clone or extract what's needed, let the guard drop, *then* `.await`. Follow that shape for anything new — holding the guard across an await point risks a deadlock the moment two commands interleave.
- **Use `tauri::async_runtime::spawn`, not raw `tokio::spawn`,** for tasks that should be tracked as part of the app's async runtime (sync loops, one-off syncs, the log flusher). It's a thin wrapper over the same tokio runtime Tauri already owns — using it consistently (rather than mixing in bare `tokio::spawn`) keeps that runtime ownership unambiguous.
- **Subprocess binaries: absolute paths, argv arrays, never a shell string.** `rsync/binary.rs` resolves rsync from a hardcoded candidate list of absolute paths specifically to avoid `Command::new("rsync")`'s PATH search; `connection.rs::ssh_path()` does the same for `ssh` (prioritizing `/usr/bin/ssh` specifically, since on a current macOS that path is SIP-protected and can't be shadowed even by root). Args are always passed as `.args(&[...])`, never interpolated into a string handed to `sh -c`. Follow the same absolute-path pattern for any new subprocess.
- **Tests are colocated and named as sentences.** Every module with logic worth protecting has a `#[cfg(test)] mod tests` at the bottom, with test function names that read as a claim about behavior (`a_leading_dash_is_refused_everywhere_ssh_would_read_it_as_an_option`, `a_stale_drag_can_never_lose_anything`). Tests target the plain functions behind the command wrappers, not the `#[tauri::command]` functions themselves. Match this shape for new logic rather than adding a separate `tests/` integration-test tree.
