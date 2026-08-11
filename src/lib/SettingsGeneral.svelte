<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "$lib/api";
  import { applyTheme } from "$lib/theme";
  import type { Settings, Theme } from "$lib/types";

  let settings = $state<Settings | null>(null);
  let error = $state("");
  let isDev = $state(false);

  const BUILT_IN: { value: Theme; label: string }[] = [
    { value: "system", label: "System" },
    { value: "light", label: "Light" },
    { value: "dark", label: "Dark" }
  ];

  const CATPPUCCIN: { value: Theme; label: string }[] = [
    { value: "catppuccin-latte", label: "Latte" },
    { value: "catppuccin-frappe", label: "Frappé" },
    { value: "catppuccin-macchiato", label: "Macchiato" },
    { value: "catppuccin-mocha", label: "Mocha" }
  ];

  // Mirrors settings.theme for the select. Kept in step explicitly at each
  // point settings changes rather than through an effect, which would fight
  // the binding and snap the dropdown back while the user was choosing.
  let theme = $state<Theme>("system");

  onMount(() => {
    api.getSettings().then((value) => {
      settings = value;
      theme = value.theme;
    });
    api.getAppInfo().then((info) => (isDev = info.isDev));
  });

  async function update(patch: Partial<Settings>) {
    if (!settings) return;
    const previous = settings;
    const next = { ...settings, ...patch };
    settings = next;
    // Theme is applied straight away rather than waiting for the round trip,
    // so the click feels instant.
    if (patch.theme) applyTheme(patch.theme);
    error = "";
    try {
      settings = await api.saveSettings(next);
      theme = settings.theme;
    } catch (err) {
      error = String(err);
      settings = previous;
      theme = previous.theme;
      applyTheme(previous.theme);
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
    <span class="title">Appearance</span>
    <small>System follows your macOS light/dark setting.</small>
    <select class="narrow-wide" bind:value={theme} onchange={() => update({ theme })}>
      <optgroup label="Built-in">
        {#each BUILT_IN as option (option.value)}
          <option value={option.value}>{option.label}</option>
        {/each}
      </optgroup>
      <optgroup label="Catppuccin">
        {#each CATPPUCCIN as option (option.value)}
          <option value={option.value}>{option.label}</option>
        {/each}
      </optgroup>
    </select>
  </div>

  {#if error}
    <p class="alert">{error}</p>
  {/if}
{:else}
  <p class="muted-line">Loading…</p>
{/if}
