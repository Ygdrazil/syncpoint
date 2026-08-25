<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import AboutView from "$lib/AboutView.svelte";
  import ProfilesView from "$lib/ProfilesView.svelte";
  import SettingsView from "$lib/SettingsView.svelte";

  // Every window loads the same SPA and tells itself apart by label. Using
  // routes instead would mean deep links that have to survive static export,
  // where only index.html is emitted — this behaves the same in dev and in a
  // bundled app.
  const label = getCurrentWindow().label;
</script>

{#if label === "settings"}
  <SettingsView />
{:else if label === "about"}
  <AboutView />
{:else}
  <ProfilesView />
{/if}
