<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import * as api from "$lib/api";
  import type { Settings } from "$lib/types";

  let settings = $state<Settings | null>(null);
  let error = $state("");
  let isDev = $state(false);

  onMount(() => {
    api.getSettings().then((value) => (settings = value));
    api.getAppInfo().then((info) => (isDev = info.isDev));
    // The What's new popup can change a setting shown here while this is open.
    const unlisten = listen("config-changed", () => {
      api.getSettings().then((value) => (settings = value));
    });
    return () => {
      unlisten.then((stop) => stop());
    };
  });

  async function update(patch: Partial<Settings>) {
    if (!settings) return;
    const previous = settings;
    const next = { ...settings, ...patch };
    settings = next;
    error = "";
    try {
      settings = await api.saveSettings(next);
    } catch (err) {
      error = String(err);
      settings = previous;
    }
  }
</script>

<h2>General</h2>

{#if settings}
  <div class="setting">
    <span class="title">Open at startup</span>
    <label class="check">
      <input
        type="checkbox"
        checked={settings.openAtStartup}
        onchange={(e) => update({ openAtStartup: e.currentTarget.checked })}
      />
      <span>
        Launch SyncPoint when you log in
        <small>Enabled profiles resume watching automatically.</small>
      </span>
    </label>
    {#if isDev}
      <small>
        This is a development build, so it registers the <code>tauri dev</code> binary
        rather than an installed app.
      </small>
    {/if}
  </div>

  <div class="setting">
    <span class="title">What's new</span>
    <label class="check">
      <input
        type="checkbox"
        checked={settings.showWhatsNew}
        onchange={(e) => update({ showWhatsNew: e.currentTarget.checked })}
      />
      <span>
        Show what changed after an update
        <small>A list of what was added, changed or fixed, the first time a new version opens.</small>
      </span>
    </label>
  </div>

  {#if error}
    <p class="alert">{error}</p>
  {/if}
{:else}
  <p class="muted-line">Loading…</p>
{/if}
