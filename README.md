# SyncPoint

Desktop app for keeping local folders mirrored to remote hosts over SSH. You set
up a profile per folder; whenever anything changes inside it, rsync runs.

Tauri 2 + SvelteKit (Svelte 5) frontend, Rust backend.

## How the syncing works

rsync has no watch mode and no daemon that watches — `rsyncd` is a *receiving*
server, not a file watcher. So the app does what lsyncd does:

```
FSEvents watcher  →  debounce (quiet period)  →  one-shot `rsync` over SSH
```

- **Watcher** — `notify` in recursive mode on the profile's source directory.
  Access-only events are dropped, and paths matching the profile's excludes
  never reach the sync loop.
- **Debounce** — a change starts a quiet period (500 ms by default); every
  further event restarts it. A save storm or a build writing an output
  directory collapses into a single rsync run instead of hundreds.
- **Coalescing** — changes landing *while* rsync is running stay queued, so the
  loop immediately does another pass. The remote never sits stale.
- **Reconcile on start** — enabling a profile, and launching the app, both queue
  an immediate sync. A watcher only reports changes from the moment it starts,
  so without this any edit made while the app was closed would never be sent
  while the UI claimed to be watching.
- **Progress** — with rsync 3.1+, `--info=progress2` gives one percentage for
  the whole transfer. Those updates arrive as carriage-return redraws rather
  than lines, so output is split on `\r` as well as `\n`; progress is routed to
  its own event instead of the log, which it would otherwise flood. openrsync
  has no equivalent, and the UI says so rather than showing a stuck bar.
- **Connection reuse** — every run passes `ControlMaster=auto` +
  `ControlPersist=300`, so the SSH handshake (~0.5–1 s) is paid once rather than
  on every sync. For the small frequent syncs a watcher produces, this is the
  difference that matters.

`BatchMode=yes` is always on: a missing key or unknown host fails immediately
with a readable error instead of blocking forever on a prompt nobody can see.

## rsync binary

Checked in order, first real rsync 3.x wins:

1. `/opt/homebrew/bin/rsync`
2. `/usr/local/bin/rsync`
3. `/usr/bin/rsync`

macOS ships Apple's **openrsync**, which speaks protocol 29 and lacks most
rsync 3.x flags. The header shows which one is in use. Only flags present in
rsync 2.6.9 are used, so the same command works on both. `brew install rsync`
for the real thing.

## Profile actions

Right-clicking a profile in the sidebar gives Edit, Duplicate and Delete.
Duplicate opens a copy in the editor rather than saving it immediately: a
duplicate shares the source and remote path, so it wants adjusting first, and it
arrives disabled so saving cannot start a second watcher on the same folder by
surprise. Delete asks for confirmation in a modal and touches no files, local or
remote.

## Connections and profiles

A **connection** is a named SSH destination — name, user, host, port, and an
optional private key path. Define it once, use it from any number of profiles.
It is SyncPoint's own record: it never reads or writes `~/.ssh/config`.

Connections live under **SSH Connections** in the Settings window, opened from
the app menu (`SyncPoint > Settings…` on macOS, `File > Settings…` elsewhere),
with <kbd>⌘,</kbd>, or from the button in the main window's header. Every window
loads the same SPA and tells itself apart by window label, so no route has to
survive static export. Edits in any window broadcast a `config-changed` event
that the others pick up.

## Icons

The app icon is the sync glyph — two arcs circling with arrowheads — in white on
a blue rounded tile, matching the accent colour used throughout the UI. The menu
bar icon reuses the same ring so the two read as one app.

Both are generated from geometry rather than drawn, since no image tooling is
assumed. To change the mark, edit the script and regenerate:

```bash
python3 scripts/generate-app-icon.py assets/logo.png
```

```bash
pnpm tauri icon assets/logo.png
```

`tauri icon` also emits iOS and Android sets; this is a desktop-only app, so
`src-tauri/icons/ios` and `src-tauri/icons/android` are deleted after running it.
It leaves the tray PNGs alone.

## Window and quit behaviour

Closing the main window **hides** it rather than quitting — the point of the
tray icon is that watchers keep running with no window open. Bring it back from
the tray (left click, or Open SyncPoint) or by clicking the Dock icon.

Quit from the tray menu or `Quit SyncPoint` (<kbd>⌘Q</kbd>) in the app menu.

Both Quit items are custom rather than `PredefinedMenuItem::quit`, because
quitting has to set a flag first: the close handler needs to tell a real quit
from a window being dismissed, otherwise it would swallow the quit and the app
could not be closed at all. Auxiliary windows (Settings, About) close normally.

## Tray icon

A menu bar icon reflects whether *any* profile is mid-sync: a ring when idle, a
ring with a filled centre while syncing. The tooltip carries the count
("syncing 2 profiles"). Left click raises the main window; right click opens a
menu with Open SyncPoint, Settings… and Quit.

The update hangs off `engine::set_status`, the single point every state change
passes through, and skips redundant writes so a batch of profiles finishing
together does not thrash the icon.

Both icons are macOS template images — shape in the alpha channel only, so the
system recolours them for light and dark menu bars. `set_icon` clears that flag
on macOS, so `set_icon_as_template(true)` is re-asserted after every swap;
without it the icon becomes a solid black blob in a dark menu bar.

They are generated, not drawn by hand:

```bash
python3 scripts/generate-tray-icons.py src-tauri/icons
```

## Settings

**General**

- **Open at startup** — registers a macOS LaunchAgent via
  `tauri-plugin-autostart`. The OS registration is the source of truth, not the
  stored value: it can be revoked in System Settings without the app knowing, so
  the checkbox reports what is actually registered. Under `tauri dev` this
  registers the dev binary, which the pane says out loud.
- **Appearance** — System, Light, Dark, or any of the four Catppuccin flavours
  (Latte, Frappé, Macchiato, Mocha), each shown with its own colour swatch.

  The Catppuccin blocks in `src/app.css` are generated from the official
  palette, not typed in — four flavours times nine variables is thirty-six hex
  values, which is exactly how a palette quietly ends up wrong:

  ```bash
  python3 scripts/generate-catppuccin-css.py
  ```

  It rewrites only the region between the `catppuccin:begin`/`end` markers. A
  test pins every `Theme` variant to a matching CSS block, since a variant with
  no block would silently fall through to the default palette.

  An explicit choice sets `data-theme` on `<html>`; the **absence** of that
  attribute is what means "follow the system". The system-dark rule is therefore
  keyed on `:root:not([data-theme])`, so any explicit theme beats the OS
  preference and adding a theme needs no change to the existing rules.

  The window's `setTheme` is called too, since CSS cannot reach the native title
  bar or the webview's own scrollbars. That only understands light and dark, so
  every theme declares which of the two it matches — Latte is light while its
  three siblings are dark, so this cannot be inferred from the name. The call
  needs the `core:window:allow-set-theme` permission, which `core:default` does
  not include.

**SSH Connections** — see above.

**Profile defaults** — applied when a profile is created. Editing them
deliberately does not touch existing profiles, so each stays independently
tunable.

System directories are refused as destinations — `/`, `/home`, `/usr`, `/etc`,
`~` and the rest. Mirror mode is on by default, so pointing a profile at one
would delete everything in it that is not in the local folder. Only exact
matches are blocked: `/home/deploy/app` is a normal target, `/home` is not.

- **Remote path prefix** — prefilled into a new profile's remote path so only
  the last part has to be typed. Set `/srv/apps` and a new profile starts at
  `/srv/apps/`. Must be absolute or `~/`-relative; empty means no prefill. It is
  stored without a trailing slash and the separator is added when prefilling, so
  typing one or not makes no difference.
- **Local folder to browse from** — where the folder picker opens when choosing
  a source. Deliberately *not* prefilled into the field: a partial local path
  fails validation immediately, since the source must be a folder that exists.
- **Debounce** — quiet period seeded into new profiles, 50–60000 ms.
- **Excludes** — patterns copied into the profile. Defaults are `.git`,
  `.DS_Store` and `node_modules`.

**Import & export** — writes your profiles to a JSON file, and merges one back
in.

- **SSH connections are excluded unless you opt in.** They carry hostnames,
  usernames and key paths; a file you hand to someone else should not leak those
  by default. Passwords are never stored, so none can be exported.
- **Import merges, matching on id** — existing entries are updated, new ones
  added, nothing already present is removed.
- **Imported profiles always arrive disabled**, so an import can never start
  pushing files to someone else's server on its own. Profiles whose connection
  is missing from the file are listed after the import.

Buttons that are about connections (Add a connection, Manage…) open the window
directly on the SSH Connections category rather than General. A window being
created has no event listener yet, so the requested category is stashed for it
to collect on mount; an already-open window is told by event instead.

Setting a key path also sends `IdentitiesOnly=yes`, otherwise ssh still offers
every key the agent holds first and can hit `MaxAuthTries` before reaching the
one that was asked for. The key path cannot contain spaces — rsync re-splits its
`-e` value on whitespace with no way to quote it.

**Test connection** opens a real SSH session and runs `true`. It doubles as a
warm-up: it primes the ControlMaster socket, so the next sync skips the
handshake.

A **profile** is a folder to watch: name, local source, connection, remote path,
excludes, debounce, and a **mirror** toggle (`--delete`, on by default) — with
mirror on, deleting a file locally deletes it on the remote.

Connections are resolved per sync rather than captured when a watcher starts, so
editing a connection reaches every profile using it without restarting anything.

Deleting a connection disables the profiles that used it. Their settings are
kept; they show an error until a new connection is assigned.

## Storage

**Last sync** is stamped only by a transfer that actually finished — enabling a
profile no longer claims it just synced. It is shown relative ("3 min ago") with
the exact timestamp on hover.

`~/Library/Application Support/com.volodiafernandez.syncpoint/config.json`,

The app was previously called LiveSync, under the identifier
`com.volodiafernandez.livesync-next`. Since `app_config_dir()` derives from the
identifier, the first launch after the rename copies the old `config.json`
across rather than starting empty. The copy is left in place, so an older build
still finds its own config. Existing profiles and connections carry over
untouched; a **launch-at-login** registration does not, and has to be toggled
off and on once so the LaunchAgent points at the renamed app.

written atomically (temp file + rename). Profiles left enabled resume watching on
launch.

A pre-connections `profiles.json` is migrated automatically on first launch:
each distinct `user@host:port` becomes one connection, and profiles are pointed
at it. The old file is left in place untouched.

Exclude patterns are passed verbatim to `rsync --exclude`. The watcher applies
its own approximate match (leading/trailing `*` only) purely to avoid waking up
for paths rsync would skip anyway; rsync stays authoritative for what transfers.

## Versioning

The version comes from git, not from a hand-edited constant. `build.rs` runs
`git describe --tags --always`, strips the `v` prefix, and bakes the result into
the binary as `SYNCPOINT_VERSION`.

| Repo state | Version shown |
| --- | --- |
| On tag `v0.1.0` | `0.1.0` |
| 3 commits past `v0.1.0` | `0.1.0-3-gab12cd3` |
| No tags at all | short commit hash |
| No git (source tarball) | falls back to `Cargo.toml` |

Builds launched with `pnpm tauri dev` append `-dev`, so `0.1.0-dev`. The signal
is `tauri::is_dev()` — internally `!cfg!(feature = "custom-protocol")`, the
feature `tauri build` turns on and `tauri dev` does not.

To cut a release, tag it — nothing else to bump:

```bash
git tag -a v0.2.0 -m "v0.2.0"
```

Pushing that tag runs [.gitlab-ci.yml](.gitlab-ci.yml), which builds a universal
macOS `.dmg` and publishes a GitLab release. See "Releasing" below.

`build.rs` declares `rerun-if-changed` on `.git/HEAD`, `.git/refs/tags` and
`.git/packed-refs`, so a new tag re-triggers it instead of baking a stale value.
There is deliberately no `--dirty` marker: a build script only re-runs when a
tracked file changes, so it would be stale as often as it was right.

The **About window** (app menu → About SyncPoint) shows the version, whether it
is a dev build, the detected rsync binary, and the config file path, with a
Copy details button for bug reports.

## Requirements

SSH key auth must already work for each connection. Run `ssh user@host` once by
hand first to accept the host key and confirm the key is loaded — with
`BatchMode` on, the app cannot answer prompts. The Test button reports exactly
what ssh said when it fails.

## Getting logs

Every rsync line, the exact command that ran, and every status change are
written to stderr as well as the in-app Output panel. Prefix is
`[syncpoint <profile-id> <stream>]`.

**stderr is the complete transcript; the Output panel is not.** `rsync -v`
prints a line per file, so a first sync of a large tree emits tens of thousands.
Sending each one across the IPC boundary froze the webview, so output is now
batched every 100 ms and capped per batch. Anything dropped is reported inline
as `… N lines omitted` rather than vanishing quietly. The panel also keeps only
the last 500 lines. For anything where you need every line, use stderr.

During development, capture the whole session:

```bash
pnpm tauri dev 2>&1 | tee ~/syncpoint.log
```

For a bundled app, launch the binary from a terminal so stderr has somewhere to
go — the executable inside the bundle is lowercase:

```bash
/Applications/SyncPoint.app/Contents/MacOS/syncpoint 2>&1 | tee ~/syncpoint.log
```

Filter to just the failures:

```bash
grep -E "stderr|error" ~/syncpoint.log
```

For UI problems rather than sync problems, right-click in the window → Inspect
Element, and check the Console tab.

## Releasing

Pushing a `vX.Y.Z` tag whose commit is on `main` builds a universal macOS
`.dmg` and publishes a release. Both forges are wired up — use whichever the
repo lives on:

| Forge | Files | macOS runner |
| --- | --- | --- |
| GitHub | [ci.yml](.github/workflows/ci.yml), [release.yml](.github/workflows/release.yml) | **Hosted** — free on public repos, 10× minutes on private |
| GitLab | [.gitlab-ci.yml](.gitlab-ci.yml) | **You provide one** (see below) |

GitHub is the lower-friction option purely because it hosts macOS runners.

```bash
git tag -a v0.2.0 -m "v0.2.0" && git push origin v0.2.0
```

**GitHub** — `ci.yml` runs tests on `ubuntu-latest` for every push and PR;
`release.yml` runs on `macos-latest` for tags, and publishes with the
preinstalled `gh` CLI. The only third-party action used is `Swatinem/rust-cache`.

**GitLab** — three stages:

| Stage | Runner | What it does |
| --- | --- | --- |
| `test` | Docker | `cargo test` + `pnpm check`, on every MR, `main` and tag |
| `package` | **macOS** | Universal `.dmg`, uploaded to the generic package registry |
| `release` | Docker | Creates the GitLab release linking that asset |

### Why macOS builds need a Mac

Electron apps cross-build from Linux because they ship a *prebuilt* platform
binary and only repackage it. A Tauri app **is** a Rust binary linked against
WebKit and AppKit, so it must be compiled on macOS against the macOS SDK — which
Apple licenses to Apple hardware only. Tauri does not support macOS
cross-compilation, and no Docker image can substitute.

GitHub hosts macOS runners, so its workflow needs no setup. On GitLab you supply
one: the cheapest option for a personal project is registering your own Mac as a
shell-executor runner tagged `macos`. The GitLab `test` job runs on an ordinary
Docker runner meanwhile, so CI is useful before that exists.

### Details worth knowing

- **`GIT_DEPTH: 0`** is deliberate. `build.rs` derives the version from
  `git describe --tags`, and GitLab's default shallow clone can leave the tag
  unreachable — which would silently bake a bare commit hash into the release
  instead of `0.2.0`.
- **The tag is checked against `main`** with `git merge-base --is-ancestor`, so a
  tag cut from a feature branch cannot publish unmerged code.
- **Assets go to the generic package registry**, not job artifacts, because
  artifacts expire and would leave dead download links on old releases.

### Signing

Builds are unsigned, so Gatekeeper blocks them on first launch — the release
notes tell users to right-click → Open or run `xattr -cr`.

To sign and notarize, set these as masked CI variables; Tauri picks them up from
the environment with no pipeline change. Note they differ from
electron-builder's names (`CSC_LINK`, `APPLE_APP_SPECIFIC_PASSWORD`):

| Variable | Purpose |
| --- | --- |
| `APPLE_CERTIFICATE` | base64 of the `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | its password |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: …` |
| `APPLE_ID`, `APPLE_PASSWORD` | Apple ID + app-specific password, for notarization |
| `APPLE_TEAM_ID` | team identifier |

## Development

```bash
pnpm install
pnpm tauri dev
```

```bash
cd src-tauri && cargo test
```

```bash
pnpm check
```
