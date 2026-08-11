<script lang="ts">
  import { onMount } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import * as api from "$lib/api";
  import { applyTheme } from "$lib/theme";
  import "../app.scss";

  let { children } = $props();

  onMount(() => {
    const sync = () => api.getSettings().then((s) => applyTheme(s.theme));
    // Every window themes itself, and re-themes when the setting changes in
    // the settings window.
    const unlisten: Promise<UnlistenFn> = listen("config-changed", sync);
    sync();
    return () => unlisten.then((fn) => fn());
  });
</script>

{@render children()}
