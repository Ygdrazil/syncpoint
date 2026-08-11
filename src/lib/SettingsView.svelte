<script lang="ts">
  import { onMount } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import * as api from "$lib/api";
  import SettingsConnections from "$lib/SettingsConnections.svelte";
  import SettingsDefaults from "$lib/SettingsDefaults.svelte";
  import SettingsGeneral from "$lib/SettingsGeneral.svelte";
  import SettingsTransfer from "$lib/SettingsTransfer.svelte";

  type Category = "general" | "connections" | "defaults" | "transfer";

  const CATEGORIES: { id: Category; label: string }[] = [
    { id: "general", label: "General" },
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
  <header>
    <h1>Settings</h1>
  </header>

  <div class="body">
    <aside>
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
    </aside>

    <main>
      {#if category === "general"}
        <SettingsGeneral />
      {:else if category === "connections"}
        <SettingsConnections />
      {:else if category === "defaults"}
        <SettingsDefaults />
      {:else}
        <SettingsTransfer />
      {/if}
    </main>
  </div>
</div>
