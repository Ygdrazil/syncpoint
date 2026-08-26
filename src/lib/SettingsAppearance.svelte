<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "$lib/api";
  import Hint from "$lib/Hint.svelte";
  import { applyTheme } from "$lib/theme";
  import type { Settings, Theme } from "$lib/types";

  let settings = $state<Settings | null>(null);
  let error = $state("");

  // Mirrors settings.theme for the grid. Kept in step explicitly at each
  // point settings changes rather than through an effect, which would fight
  // the click and snap the selection back while the save is in flight.
  let theme = $state<Theme>("system");

  onMount(() => {
    api.getSettings().then((value) => {
      settings = value;
      theme = value.theme;
    });
  });

  type Option = { value: Theme; label: string };

  const BUILT_IN: Option[] = [
    { value: "system", label: "System" },
    { value: "light", label: "Light" },
    { value: "dark", label: "Dark" }
  ];

  const CATPPUCCIN: Option[] = [
    { value: "catppuccin-latte", label: "Latte" },
    { value: "catppuccin-frappe", label: "Frappé" },
    { value: "catppuccin-macchiato", label: "Macchiato" },
    { value: "catppuccin-mocha", label: "Mocha" }
  ];

  const DRACULA: Option[] = [{ value: "dracula", label: "Dracula" }];

  async function select(value: Theme) {
    if (!settings || value === theme) return;
    const previous = settings;
    theme = value;
    settings = { ...settings, theme: value };
    // Applied straight away rather than waiting for the round trip, so the
    // click feels instant.
    applyTheme(value);
    error = "";
    try {
      settings = await api.saveSettings(settings);
    } catch (err) {
      error = String(err);
      settings = previous;
      theme = previous.theme;
      applyTheme(previous.theme);
    }
  }

  type LayoutOption = { value: boolean; label: string };

  const LAYOUTS: LayoutOption[] = [
    { value: false, label: "Normal" },
    { value: true, label: "Compact" }
  ];

  async function selectCompact(value: boolean) {
    if (!settings || value === settings.compact) return;
    const previous = settings;
    settings = { ...settings, compact: value };
    error = "";
    try {
      settings = await api.saveSettings(settings);
    } catch (err) {
      error = String(err);
      settings = previous;
    }
  }
</script>

{#snippet group(options: Option[])}
  <div class="picker-grid">
    {#each options as option (option.value)}
      <button
        type="button"
        class="picker-card"
        class:active={theme === option.value}
        onclick={() => select(option.value)}
      >
        <span class="theme-swatch" data-theme={option.value}>
          <span class="swatch-dot text"></span>
          <span class="swatch-dot ok"></span>
          <span class="swatch-dot danger"></span>
        </span>
        <span class="picker-card-label">{option.label}</span>
      </button>
    {/each}
  </div>
{/snippet}

<h2>Appearance</h2>

{#if settings}
  <div class="setting">
    <span class="title">
      Main window
      <Hint label="About compact mode">
        Compact hides the detail pane and narrows the window to the profile
        list — everything a profile needs is on its right-click menu. Opening
        the profile editor brings the pane back, and turns this off with it.
      </Hint>
    </span>
    <div class="picker-grid">
      {#each LAYOUTS as option (String(option.value))}
        <button
          type="button"
          class="picker-card"
          class:active={settings.compact === option.value}
          onclick={() => selectCompact(option.value)}
        >
          <span class="window-mock" class:compact={option.value}>
            <span class="window-mock-sidebar">
              <span class="mock-row"></span>
              <span class="mock-row"></span>
              <span class="mock-row"></span>
            </span>
            {#if !option.value}
              <span class="window-mock-pane">
                <span class="mock-line"></span>
                <span class="mock-line short"></span>
              </span>
            {/if}
          </span>
          <span class="picker-card-label">{option.label}</span>
        </button>
      {/each}
    </div>
  </div>

  <div class="setting">
    <span class="title">Theme</span>
    {@render group(BUILT_IN)}

    <p class="picker-group-label">Catppuccin</p>
    {@render group(CATPPUCCIN)}

    <p class="picker-group-label">Dracula</p>
    {@render group(DRACULA)}
  </div>

  {#if error}
    <p class="alert">{error}</p>
  {/if}
{:else}
  <p class="muted-line">Loading…</p>
{/if}
