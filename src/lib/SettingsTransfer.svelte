<script lang="ts">
  import { open, save } from "@tauri-apps/plugin-dialog";
  import * as api from "$lib/api";
  import Hint from "$lib/Hint.svelte";
  import type { ImportSummary } from "$lib/types";

  // Off by default: connections carry hostnames, usernames and key paths, which
  // is infrastructure detail a file you hand to someone else should not leak
  // unless you meant it to.
  let includeConnections = $state(false);
  let busy = $state(false);
  let error = $state("");
  let notice = $state("");
  let summary = $state<ImportSummary | null>(null);

  function defaultName(): string {
    const today = new Date().toISOString().slice(0, 10);
    return `syncpoint-${today}.json`;
  }

  async function exportConfig() {
    error = "";
    notice = "";
    summary = null;
    const path = await save({
      defaultPath: defaultName(),
      filters: [{ name: "SyncPoint export", extensions: ["json"] }]
    });
    if (!path) return;

    busy = true;
    try {
      const count = await api.exportConfig(path, includeConnections);
      notice = `Exported ${count} profile(s)${
        includeConnections ? " with connections" : " without connections"
      } to ${path}.`;
    } catch (err) {
      error = String(err);
    } finally {
      busy = false;
    }
  }

  async function importConfig() {
    error = "";
    notice = "";
    summary = null;
    const path = await open({
      multiple: false,
      filters: [{ name: "SyncPoint export", extensions: ["json"] }]
    });
    if (typeof path !== "string") return;

    busy = true;
    try {
      summary = await api.importConfig(path);
    } catch (err) {
      error = String(err);
    } finally {
      busy = false;
    }
  }
</script>

<h2>Import &amp; export</h2>
<p class="muted-line">
  Move your setup between machines, or keep a backup of it. Nothing here touches
  files on your Mac or on any remote host.
</p>

<div class="setting">
  <span class="title">
    Export
    <Hint label="About exporting">
      Leave connections out if the file is going anywhere you would not paste a
      server address. Passwords are never stored, so none can be exported.
    </Hint>
  </span>
  <small>Writes a JSON file containing your profiles.</small>
  <label class="check">
    <input type="checkbox" bind:checked={includeConnections} />
    <span>
      Include SSH connections
      <small>They carry hostnames, usernames and key paths.</small>
    </span>
  </label>
  <span class="row">
    <button class="primary" disabled={busy} onclick={exportConfig}>Export…</button>
  </span>
</div>

<div class="setting">
  <span class="title">
    Import
    <Hint label="How importing merges">
      Merges the file into what you already have, matching on id: existing
      entries are updated, new ones added, and nothing already here is removed.
      If it replaces a connection an existing profile was using, that profile is
      switched off too.
    </Hint>
  </span>
  <small>
    Imported profiles always arrive <strong>paused</strong>, so an import never
    starts sending files on its own — review each one, then enable it.
  </small>
  <span class="row">
    <button disabled={busy} onclick={importConfig}>Import…</button>
  </span>
</div>

{#if error}
  <p class="alert">{error}</p>
{/if}

{#if notice}
  <p class="alert ok">{notice}</p>
{/if}

{#if summary}
  <div class="setting">
    <span class="title">Imported</span>
    <dl class="facts">
      <div><dt>Profiles added</dt><dd>{summary.profilesAdded}</dd></div>
      <div><dt>Profiles updated</dt><dd>{summary.profilesUpdated}</dd></div>
      <div><dt>Connections added</dt><dd>{summary.connectionsAdded}</dd></div>
      <div><dt>Connections updated</dt><dd>{summary.connectionsUpdated}</dd></div>
    </dl>
    {#if summary.profilesDisabled > 0}
      <p class="alert">
        {summary.profilesDisabled} running profile(s) were paused: this file
        replaced the connection they send through, so their destination changed.
        Check where they now point before enabling them again.
      </p>
    {/if}
    {#if summary.profilesMissingConnection.length > 0}
      <p class="alert">
        These profiles have no matching connection, so they cannot run until one
        is assigned — most likely the file was exported without connections:
        {summary.profilesMissingConnection.join(", ")}
      </p>
    {/if}
  </div>
{/if}
