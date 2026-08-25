# SyncPoint

Keep a local folder mirrored on a remote host over SSH. Edit a file, it lands on the server a moment later.

A macOS menu bar app built with Tauri, Svelte and rsync.

## Requirements

- macOS
- SSH key authentication working for each destination (`ssh user@host` once to accept the host key)
- `rsync` — macOS ships one, but for transfer progress install a current release:

  ```bash
  brew install rsync
  ```

## Install

Download the latest `.dmg` from [Releases](https://github.com/Ygdrazil/syncpoint/releases).

Builds are unsigned. To open on first launch, go to **System Settings → Privacy & Security → Open Anyway**, or clear the quarantine flag:

```bash
xattr -dr com.apple.quarantine /Applications/SyncPoint.app
```

### Build from source

Requires Node.js 22+, pnpm, and a Rust toolchain.

```bash
pnpm install
pnpm tauri build
```

The app lands in `src-tauri/target/release/bundle/`. For a universal (Apple Silicon + Intel) build:

```bash
pnpm tauri build --target universal-apple-darwin
```

## Usage

1. **Settings → SSH Connections → New connection** — fill in host and user, hit **Test**.
2. **+ in the sidebar → New profile** — pick a local folder, choose the connection, set the remote path.
3. Leave **Watch this folder** ticked and save. The first sync starts immediately.

Closing the window keeps syncing — quit from the menu bar icon.

## Development

```bash
pnpm install
pnpm tauri dev
```

Tests and type checks:

```bash
cargo test --lib --manifest-path src-tauri/Cargo.toml
pnpm check
```

## License

MIT
