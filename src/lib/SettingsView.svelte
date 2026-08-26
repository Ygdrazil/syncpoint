<script lang="ts">
  import { onMount } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import * as api from "$lib/api";
  import SettingsAppearance from "$lib/SettingsAppearance.svelte";
  import SettingsConnections from "$lib/SettingsConnections.svelte";
  import SettingsDefaults from "$lib/SettingsDefaults.svelte";
  import SettingsGeneral from "$lib/SettingsGeneral.svelte";
  import SettingsTransfer from "$lib/SettingsTransfer.svelte";

  type Category = "general" | "appearance" | "connections" | "defaults" | "transfer";

  const CATEGORIES: { id: Category; label: string }[] = [
    { id: "general", label: "General" },
    { id: "appearance", label: "Appearance" },
    { id: "connections", label: "SSH Connections" },
    { id: "defaults", label: "Profile defaults" },
    { id: "transfer", label: "Import & export" }
  ];

  let category = $state<Category>("general");

  function select(value: string | null) {
    if (CATEGORIES.some((c) => c.id === value)) category = value as Category;
  }

  onMount(() => {
    // Two paths, because a window being created has no listener yet: a fresh
    // window collects the stashed section here, an already-open one is told by
    // event.
    api.takeSettingsSection().then(select);
    const unlisten: Promise<UnlistenFn> = listen<string>("settings-navigate", (event) =>
      select(event.payload)
    );
    return () => unlisten.then((fn) => fn());
  });
</script>

<div class="app">
  <aside>
    <div class="side-head" data-tauri-drag-region>
      <div class="side-title" data-tauri-drag-region>
        <h1 data-tauri-drag-region>Settings</h1>
      </div>
    </div>

    <div class="side-scroll">
      <ul class="items">
        {#each CATEGORIES as item (item.id)}
          <li>
            <button
              class="category"
              class:active={category === item.id}
              onclick={() => (category = item.id)}
            >
              {item.label}
            </button>
          </li>
        {/each}
      </ul>
    </div>
  </aside>

  <main>
    <div class="main-head" data-tauri-drag-region></div>

    <div class="pane">
      {#if category === "general"}
        <SettingsGeneral />
      {:else if category === "appearance"}
        <SettingsAppearance />
      {:else if category === "connections"}
        <SettingsConnections />
      {:else if category === "defaults"}
        <SettingsDefaults />
      {:else}
        <SettingsTransfer />
      {/if}
    </div>
  </main>
</div>
