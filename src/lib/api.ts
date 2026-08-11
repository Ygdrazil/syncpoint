import { invoke } from "@tauri-apps/api/core";
import type {
  AppInfo,
  Connection,
  ImportSummary,
  Profile,
  RsyncInfo,
  Settings,
  SyncStatus
} from "./types";

export const listProfiles = () => invoke<Profile[]>("list_profiles");
export const listConnections = () => invoke<Connection[]>("list_connections");
export const listStatuses = () => invoke<SyncStatus[]>("list_statuses");
export const getRsyncInfo = () => invoke<RsyncInfo | null>("rsync_info");

export const getAppInfo = () => invoke<AppInfo>("app_info");
export type SettingsSection = "general" | "connections" | "defaults" | "transfer";

export const openSettings = (section?: SettingsSection) =>
  invoke<void>("open_settings", { section });
export const takeSettingsSection = () => invoke<string | null>("take_settings_section");

export const exportConfig = (path: string, includeConnections: boolean) =>
  invoke<number>("export_config", { path, includeConnections });
export const importConfig = (path: string) => invoke<ImportSummary>("import_config", { path });

export const getSettings = () => invoke<Settings>("get_settings");
export const saveSettings = (incoming: Settings) => invoke<Settings>("save_settings", { incoming });

export const saveConnection = (incoming: Connection) =>
  invoke<Connection>("save_connection", { incoming });
export const deleteConnection = (id: string) => invoke<string[]>("delete_connection", { id });
export const testConnection = (candidate: Connection) =>
  invoke<string>("test_connection", { candidate });

export const saveProfile = (incoming: Profile) => invoke<Profile>("save_profile", { incoming });
export const deleteProfile = (id: string) => invoke<void>("delete_profile", { id });
export const setProfileEnabled = (id: string, enabled: boolean) =>
  invoke<void>("set_profile_enabled", { id, enabled });
export const syncNow = (id: string) => invoke<void>("sync_now", { id });
