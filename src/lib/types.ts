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
  delete: boolean;
  excludes: string[];
  debounceMs: number;
};

export type SyncState = "disabled" | "idle" | "pending" | "syncing" | "error";

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
};

export type ImportSummary = {
  profilesAdded: number;
  profilesUpdated: number;
  connectionsAdded: number;
  connectionsUpdated: number;
  /// Imported profiles whose connection is not present — usually because the
  /// file was exported without connections.
  profilesMissingConnection: string[];
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
    debounceMs
  };
}

export function connectionTarget(connection: Connection): string {
  return connection.user ? `${connection.user}@${connection.host}` : connection.host;
}
