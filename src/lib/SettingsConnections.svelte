<script lang="ts">
  import { onMount } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import * as api from "$lib/api";
  import ConfirmDialog from "$lib/ConfirmDialog.svelte";
  import { connectionTarget, emptyConnection, type Connection, type Profile } from "$lib/types";

  let connections = $state<Connection[]>([]);
  let profiles = $state<Profile[]>([]);
  let draft = $state<Connection | null>(null);
  let formError = $state("");
  let testState = $state<{ busy: boolean; ok: boolean; message: string } | null>(null);
  let notice = $state("");
  let pendingDelete = $state<Connection | null>(null);

  function profilesUsing(connectionId: string): Profile[] {
    return profiles.filter((p) => p.connectionId === connectionId);
  }

  async function refresh() {
    connections = await api.listConnections();
    profiles = await api.listProfiles();
  }

  onMount(() => {
    // The main window can disable profiles or change things too.
    const unlisten: Promise<UnlistenFn> = listen("config-changed", () => refresh());
    refresh();
    return () => unlisten.then((fn) => fn());
  });

  function startNew() {
    draft = emptyConnection();
    formError = "";
    testState = null;
  }

  function startEdit(connection: Connection) {
    draft = { ...connection };
    formError = "";
    testState = null;
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (!draft) return;
    formError = "";
    try {
      await api.saveConnection(draft);
      draft = null;
      await refresh();
    } catch (err) {
      formError = String(err);
    }
  }

  async function test(candidate: Connection) {
    testState = { busy: true, ok: false, message: "Connecting…" };
    try {
      testState = { busy: false, ok: true, message: await api.testConnection(candidate) };
    } catch (err) {
      testState = { busy: false, ok: false, message: String(err) };
    }
  }

  /// The profiles that would stop if a connection went away, listed in the
  /// dialog so the consequence is visible before confirming rather than after.
  function affectedBy(connection: Connection): string {
    const affected = profilesUsing(connection.id);
    if (affected.length === 0) return "";
    return affected.map((p) => `· ${p.name}`).join("\n");
  }

  async function confirmDelete() {
    const connection = pendingDelete;
    pendingDelete = null;
    if (!connection) return;

    const disabled = await api.deleteConnection(connection.id);
    notice = disabled.length
      ? `Deleted "${connection.name}". Disabled: ${disabled.join(", ")}.`
      : `Deleted "${connection.name}".`;
    await refresh();
  }
</script>

{#if pendingDelete}
  <ConfirmDialog
    title="Delete “{pendingDelete.name}”?"
    message={profilesUsing(pendingDelete.id).length
      ? `${profilesUsing(pendingDelete.id).length} profile(s) use it and will be disabled. Their settings are kept — assign a new connection afterwards.`
      : "No profile uses this connection."}
    detail={affectedBy(pendingDelete)}
    confirmLabel="Delete connection"
    onconfirm={confirmDelete}
    oncancel={() => (pendingDelete = null)}
  />
{/if}

{#if draft}
  <form onsubmit={save}>
    <h2>{draft.id ? "Edit connection" : "New connection"}</h2>

    <label>
      Name
      <input bind:value={draft.name} placeholder="Production server" required autocapitalize="off" autocorrect="off" spellcheck="false" />
      <small>Whatever you want to call it in SyncPoint.</small>
    </label>

    <span class="row">
      <label class="grow">
        User
        <input bind:value={draft.user} placeholder="deploy" autocapitalize="off" autocorrect="off" spellcheck="false" />
      </label>
      <label class="grow">
        Host
        <input bind:value={draft.host} placeholder="server.example.com" required autocapitalize="off" autocorrect="off" spellcheck="false" />
      </label>
      <label class="port">
        Port
        <input type="number" bind:value={draft.port} min="1" max="65535" />
      </label>
    </span>

    <label>
      Private key (optional)
      <input bind:value={draft.identityFile} placeholder="~/.ssh/id_rsa" autocapitalize="off" autocorrect="off" spellcheck="false" />
      <small>Leave empty to let ssh pick as it normally does. No spaces in the path.</small>
    </label>

    {#if testState}
      <p class="alert" class:ok={testState.ok}>{testState.message}</p>
    {/if}
    {#if formError}
      <p class="alert">{formError}</p>
    {/if}

    <span class="row end">
      <button type="button" disabled={testState?.busy} onclick={() => test(draft!)}>
        {testState?.busy ? "Testing…" : "Test"}
      </button>
      <button type="button" onclick={() => (draft = null)}>Cancel</button>
      <button type="submit" class="primary">Save</button>
    </span>
  </form>
{:else}
  <div class="detail-head">
    <h2>SSH Connections</h2>
    <button class="primary" onclick={startNew}>New connection</button>
  </div>

  {#if notice}
    <p class="muted-line">
      {notice}
      <button class="link" onclick={() => (notice = "")}>dismiss</button>
    </p>
  {/if}

  {#if testState}
    <p class="alert" class:ok={testState.ok}>{testState.message}</p>
  {/if}

  {#if connections.length === 0}
    <p class="muted-line">
      No connections yet. Add one, then profiles in the main window can use it.
    </p>
  {:else}
    {#each connections as connection (connection.id)}
      {@const used = profilesUsing(connection.id)}
      <div class="conn-row">
        <span class="grow">
          <strong>{connection.name}</strong>
          <span class="path">{connectionTarget(connection)}:{connection.port}</span>
          <small>
            {used.length === 0
              ? "Not used by any profile"
              : `Used by ${used.map((p) => p.name).join(", ")}`}
          </small>
        </span>
        <button disabled={testState?.busy} onclick={() => test(connection)}>Test</button>
        <button onclick={() => startEdit(connection)}>Edit</button>
        <button class="danger" onclick={() => (pendingDelete = connection)}>Delete</button>
      </div>
    {/each}
  {/if}
{/if}
