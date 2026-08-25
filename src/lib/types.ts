export type Connection = {
  id: string;
  name: string;
  user: string;
  host: string;
  port: number;
  identityFile: string;
};

export type Profile = {
  id: string;
  name: string;
  source: string;
  connectionId: string;
  remotePath: string;
  enabled: boolean;
  /// Make the remote an exact copy: anything not sent is removed there,
  /// excluded files included.
  delete: boolean;
  excludes: string[];
  debounceMs: number;
  /// Containing folder, or null at the top level.
  folderId: string | null;
  /// Place within the folder, in an order shared with the folders at the same
  /// level. Assigned by the backend; only a drag changes it.
  position: number;
};

export type Folder = {
  id: string;
  name: string;
  /// Containing folder, or null at the top level.
  parentId: string | null;
  /// Place within the parent, in an order shared with the profiles at the same
  /// level. Assigned by the backend; only a drag changes it.
  position: number;
};

export type ItemKind = "folder" | "profile";

/// One transient message in the bottom-right stack. `id` rather than the text
/// as the key, so two identical messages do not collapse into one.
export type Toast = {
  id: number;
  text: string;
  tone: "info" | "error";
  /// Lifetime in seconds, for the countdown bar. Absent when the toast stays
  /// until it is dismissed.
  seconds?: number;
  /// Optional action button on the toast.
  action?: {
    label: string;
    onclick: () => void;
  };
};

/// What deleting a folder took with it.
export type FolderDeletion = {
  profiles: number;
  /// Nested folders, not counting the one that was deleted.
  folders: number;
};

export type SyncState = "disabled" | "idle" | "pending" | "syncing" | "error" | "paused";

export type SyncStatus = {
  profileId: string;
  state: SyncState;
  message: string | null;
  lastSync: number | null;
};

export type LogLine = {
  line: string;
  stream: "stdout" | "stderr" | "meta";
};

/// Output arrives in periodic batches rather than one event per line, so a
/// large sync cannot flood the webview. `skipped` counts lines the backend
/// dropped to bound the payload; the full transcript is on stderr.
export type LogBatch = {
  profileId: string;
  lines: LogLine[];
  skipped: number;
};

export type SyncProgress = {
  profileId: string;
  percent: number;
  transferred: string;
  rate: string;
  eta: string;
};

export type RsyncInfo = {
  path: string;
  versionLine: string;
  isOpenrsync: boolean;
  major: number;
  supportsProgress: boolean;
};

export type Theme =
  | "system"
  | "light"
  | "dark"
  | "catppuccin-latte"
  | "catppuccin-frappe"
  | "catppuccin-macchiato"
  | "catppuccin-mocha";

export type Settings = {
  theme: Theme;
  openAtStartup: boolean;
  /// Seeded into new profiles. Editing it leaves existing profiles alone.
  defaultExcludes: string[];
  /// Prefilled into a new profile's remote path. Stored without a trailing slash.
  defaultRemotePrefix: string;
  /// Where the folder picker opens. Not prefilled into the source field.
  defaultLocalRoot: string;
  defaultDebounceMs: number;
  /// Hide the detail pane and narrow the main window to the profile list.
  compact: boolean;
  /// Global suspend; leaves each profile's own enabled flag untouched.
  paused: boolean;
};

export type ImportSummary = {
  profilesAdded: number;
  profilesUpdated: number;
  connectionsAdded: number;
  connectionsUpdated: number;
  /// Imported profiles whose connection is not present — usually because the
  /// file was exported without connections.
  profilesMissingConnection: string[];
  /// Running profiles switched off because the import replaced the connection
  /// they send through.
  profilesDisabled: number;
};

export type AppInfo = {
  name: string;
  /// From `git describe --tags`, with `-dev` appended under `tauri dev`.
  version: string;
  isDev: boolean;
  configPath: string;
  rsync: RsyncInfo | null;
};

export function emptyConnection(): Connection {
  return {
    id: "",
    name: "",
    user: "",
    host: "",
    port: 22,
    identityFile: ""
  };
}

export function emptyProfile(
  connectionId = "",
  excludes: string[] = [],
  remotePrefix = "",
  debounceMs = 500
): Profile {
  return {
    id: "",
    name: "",
    source: "",
    connectionId,
    // The stored prefix carries no trailing slash, so add the separator here.
    remotePath: remotePrefix ? `${remotePrefix}/` : "",
    enabled: true,
    delete: true,
    // Seeded from settings rather than hardcoded here.
    excludes: [...excludes],
    folderId: null,
    // The backend puts a new profile after everything already at its level.
    position: 0,
    debounceMs
  };
}

export function connectionTarget(connection: Connection): string {
  return connection.user ? `${connection.user}@${connection.host}` : connection.host;
}
