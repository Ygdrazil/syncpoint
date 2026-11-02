import { invoke } from "@tauri-apps/api/core";
import type {
  AppInfo,
  Connection,
  Folder,
  FolderDeletion,
  ImportSummary,
  ItemKind,
  Profile,
  RenderedPaths,
  RsyncInfo,
  Settings,
  SyncStatus
} from "./types";

export const listProfiles = () => invoke<Profile[]>("list_profiles");
export const listFolders = () => invoke<Folder[]>("list_folders");
export const saveFolder = (incoming: Folder) => invoke<Folder>("save_folder", { incoming });
export const deleteFolder = (id: string) =>
  invoke<FolderDeletion>("delete_folder", { id });
/// Where a drag ends: the folder it landed in, and the item of either kind it
/// landed above (`null` for last). Both halves travel together because a drop
/// decides both at once.
export const moveItem = (
  kind: ItemKind,
  id: string,
  folderId: string | null,
  before: string | null
) => invoke<void>("move_item", { kind, id, folderId, before });
export const listConnections = () => invoke<Connection[]>("list_connections");
export const listStatuses = () => invoke<SyncStatus[]>("list_statuses");
export const getRsyncInfo = () => invoke<RsyncInfo | null>("rsync_info");

export const getAppInfo = () => invoke<AppInfo>("app_info");
export type SettingsSection = "general" | "connections" | "defaults" | "transfer";

export const openSettings = (section?: SettingsSection) =>
  invoke<void>("open_settings", { section });
export const takeSettingsSection = () => invoke<string | null>("take_settings_section");
/// Narrow the main window to just its sidebar, and back.
export const setCompact = (compact: boolean) => invoke<void>("set_compact", { compact });

export const openAbout = (autoUpdate?: boolean) =>
  invoke<void>("open_about", { autoUpdate });
export const takeAutoUpdate = () => invoke<boolean | null>("take_auto_update");

export const exportConfig = (
  path: string,
  includeConnections: boolean,
  includePreferences: boolean
) => invoke<number>("export_config", { path, includeConnections, includePreferences });
export const importConfig = (path: string) => invoke<ImportSummary>("import_config", { path });

export const getSettings = () => invoke<Settings>("get_settings");
export const saveSettings = (incoming: Settings) => invoke<Settings>("save_settings", { incoming });

export const saveConnection = (incoming: Connection) =>
  invoke<Connection>("save_connection", { incoming });
export const deleteConnection = (id: string) => invoke<string[]>("delete_connection", { id });
export const testConnection = (candidate: Connection) =>
  invoke<string>("test_connection", { candidate });

export const renderProfilePaths = (draft: Profile) =>
  invoke<RenderedPaths>("render_profile_paths", { draft });
export const previewPathTemplates = (
  sourceTemplate: string,
  targetTemplate: string,
  localRoot: string
) =>
  invoke<RenderedPaths>("preview_path_templates", { sourceTemplate, targetTemplate, localRoot });

export const saveProfile = (incoming: Profile) => invoke<Profile>("save_profile", { incoming });
export const deleteProfile = (id: string) => invoke<void>("delete_profile", { id });
export const setProfileEnabled = (id: string, enabled: boolean) =>
  invoke<void>("set_profile_enabled", { id, enabled });
export const syncNow = (id: string) => invoke<void>("sync_now", { id });
export const syncAll = () => invoke<number>("sync_all");
export const setPaused = (paused: boolean) => invoke<void>("set_paused", { paused });
