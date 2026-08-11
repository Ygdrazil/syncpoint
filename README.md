# SyncPoint

[![CI](https://github.com/Ygdrazil/syncpoint/actions/workflows/ci.yml/badge.svg)](https://github.com/Ygdrazil/syncpoint/actions/workflows/ci.yml)

Keep a local folder mirrored on a remote host over SSH. Edit a file, it lands on
the server a moment later.

A macOS menu bar app built with Tauri, Svelte and rsync.

## Features

- **Watches folders** and syncs on change, with a configurable quiet period so a
  save storm or a build becomes one transfer instead of hundreds
- **Multiple profiles**, each pairing a local folder with a remote destination
- **Reusable SSH connections** shared across profiles, with an optional key path
  and a connection test
- **Mirror mode** (`rsync --delete`) per profile, with system directories
  refused as destinations
- **Per-profile excludes**, plus defaults applied to every new profile
- **Live output and transfer progress**, with a menu bar icon showing when any
  profile is syncing
- **Runs in the background** — closing the window keeps syncing
- **Launch at login**, light/dark/system themes and the four Catppuccin flavours
- **Import and export** your setup, with SSH connections excluded by default

## Requirements

- macOS
- SSH key authentication already working for each destination. Run
  `ssh user@host` once by hand to accept the host key — SyncPoint never prompts
  for passwords.
- `rsync`. macOS ships Apple's openrsync, which works but cannot report transfer
  progress. For that, install a current release:

  ```bash
  brew install rsync
  ```

## Install

### Download

Grab the latest `.dmg` from [Releases](https://github.com/Ygdrazil/syncpoint/releases).

Builds are unsigned, so macOS blocks them on first launch. Open **System
Settings → Privacy & Security → Open Anyway**, or clear the quarantine flag:

```bash
xattr -dr com.apple.quarantine /Applications/SyncPoint.app
```

### Build from source

```bash
pnpm install
```

```bash
pnpm tauri build
```

The app lands in `src-tauri/target/release/bundle/`. For a build that runs on
both Apple Silicon and Intel:

```bash
pnpm tauri build --target universal-apple-darwin
```

## Usage

1. Open **Settings** (<kbd>⌘,</kbd>) → **SSH Connections** → **New connection**.
   Fill in the host and user, then hit **Test**.
2. Back in the main window, click **New profile**. Choose a local folder, pick
   the connection, and set the remote path.
3. Leave **Watch this folder** ticked and save. The first sync starts
   immediately.

Right-click a profile to edit, duplicate or delete it. Closing the window keeps
everything running — quit from the menu bar icon.

## Settings

| Category | |
| --- | --- |
| General | Launch at login, appearance |
| SSH Connections | Named destinations shared by profiles |
| Profile defaults | Remote path prefix, source folder, debounce and excludes for new profiles |
| Import & export | Back up or move your setup between machines |

Configuration lives in
`~/Library/Application Support/com.volodiafernandez.syncpoint/config.json`.

## Development

```bash
pnpm install
```

```bash
pnpm tauri dev
```

Tests and type checks:

```bash
cargo test --lib --manifest-path src-tauri/Cargo.toml
```

```bash
pnpm check
```

Styles are SCSS under `src/styles/`, entry point `src/app.scss`.

Adding a theme is one entry in `$themes` in
[`src/styles/_themes.scss`](src/styles/_themes.scss) — the selector, the
`color-scheme` hint and the system fallback are all emitted from it. Add a
matching variant to `Theme` in `src-tauri/src/settings.rs`, an entry in
`NATIVE` in `src/lib/theme.ts`, and a label in `SettingsGeneral.svelte`. A test
fails if a variant has no palette.

To regenerate the bundle icons from `assets/logo.png`:

```bash
pnpm tauri icon assets/logo.png
```

## Releasing

The version comes from `git describe`, so tagging is the whole release process:

```bash
git tag -a v0.2.0 -m "v0.2.0" && git push origin v0.2.0
```

That builds a universal `.dmg` and publishes a GitHub release. A GitLab pipeline
(`.gitlab-ci.yml`) does the same, but needs a macOS runner of your own.

To sign and notarize, uncomment the `env` block in
[`.github/workflows/release.yml`](.github/workflows/release.yml) and add
`APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`,
`APPLE_ID`, `APPLE_PASSWORD` and `APPLE_TEAM_ID` as repository secrets.

## Troubleshooting

Every sync writes its full output to stderr as well as the in-app log:

```bash
/Applications/SyncPoint.app/Contents/MacOS/syncpoint 2>&1 | tee ~/syncpoint.log
```

## License

MIT
