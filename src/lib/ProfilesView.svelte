<script lang="ts">
  import { onMount } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { open } from "@tauri-apps/plugin-dialog";
  import * as api from "$lib/api";
  import ConfirmDialog from "$lib/ConfirmDialog.svelte";
  import {
    connectionTarget,
    emptyProfile,
    type Connection,
    type LogBatch,
    type LogLine,
    type Profile,
    type RsyncInfo,
    type Settings,
    type SyncProgress,
    type SyncState,
    type SyncStatus
  } from "$lib/types";

  const MAX_LOG_LINES = 500;

  let profiles = $state<Profile[]>([]);
  let connections = $state<Connection[]>([]);
  let statuses = $state<Record<string, SyncStatus>>({});
  let logs = $state<Record<string, LogLine[]>>({});
  let rsync = $state<RsyncInfo | null>(null);
  let settings = $state<Settings | null>(null);
  let progress = $state<Record<string, SyncProgress>>({});
  // Ticks so relative "last sync" labels do not go stale on screen.
  let now = $state(Date.now());

  let selectedId = $state<string | null>(null);
  let draft = $state<Profile | null>(null);
  let formError = $state("");
  let excludesText = $state("");

  let selected = $derived(profiles.find((p) => p.id === selectedId) ?? null);
  let selectedLog = $derived(selectedId ? (logs[selectedId] ?? []) : []);

  const STATE_LABEL: Record<SyncState, string> = {
    disabled: "Disabled",
    idle: "Watching",
    pending: "Changes queued",
    syncing: "Syncing",
    error: "Error"
  };

  function stateOf(id: string): SyncState {
    return statuses[id]?.state ?? "disabled";
  }

  function connectionOf(profile: Profile): Connection | null {
    return connections.find((c) => c.id === profile.connectionId) ?? null;
  }

  async function refresh() {
    profiles = await api.listProfiles();
    connections = await api.listConnections();
    settings = await api.getSettings();
    const list = await api.listStatuses();
    statuses = Object.fromEntries(list.map((s) => [s.profileId, s]));
    if (selectedId && !profiles.some((p) => p.id === selectedId)) selectedId = null;
    if (!selectedId && profiles.length > 0) selectedId = profiles[0].id;
  }

  function setError(id: string, err: unknown) {
    statuses[id] = {
      profileId: id,
      state: "error",
      message: String(err),
      lastSync: statuses[id]?.lastSync ?? null
    };
  }

  onMount(() => {
    const unlisteners: Promise<UnlistenFn>[] = [
      listen<SyncStatus>("sync-status", (event) => {
        statuses[event.payload.profileId] = event.payload;
        // A finished or failed run leaves no progress to show.
        if (event.payload.state !== "syncing") delete progress[event.payload.profileId];
      }),
      listen<SyncProgress>("sync-progress", (event) => {
        progress[event.payload.profileId] = event.payload;
      }),
      listen<LogBatch>("sync-log", (event) => {
        const { profileId, lines, skipped } = event.payload;
        if (!logs[profileId]) logs[profileId] = [];
        const buffer = logs[profileId];

        if (skipped > 0) {
          buffer.push({
            stream: "meta",
            line: `… ${skipped.toLocaleString()} lines omitted (full output on stderr)`
          });
        }
        // Appended in place: rebuilding the array per line is what froze the
        // UI during a large first sync.
        for (const entry of lines) buffer.push(entry);
        // Cap the buffer so a long-running profile cannot grow without bound.
        if (buffer.length > MAX_LOG_LINES) buffer.splice(0, buffer.length - MAX_LOG_LINES);
      }),
      // Connections live in the settings window; pick up its edits.
      listen("config-changed", () => refresh())
    ];

    api.getRsyncInfo().then((info) => (rsync = info));
    refresh();

    const tick = setInterval(() => (now = Date.now()), 30_000);
    return () => {
      clearInterval(tick);
      unlisteners.forEach((u) => u.then((fn) => fn()));
    };
  });

  function startNew() {
    draft = emptyProfile(
      connections[0]?.id ?? "",
      settings?.defaultExcludes ?? [],
      settings?.defaultRemotePrefix ?? "",
      settings?.defaultDebounceMs ?? 500
    );
    excludesText = draft.excludes.join("\n");
    formError = "";
  }

  function startEdit(profile: Profile) {
    draft = { ...profile, excludes: [...profile.excludes] };
    excludesText = profile.excludes.join("\n");
    formError = "";
  }

  async function pickSource() {
    const picked = await open({
      directory: true,
      multiple: false,
      // Opens where the user keeps their projects instead of wherever the
      // system last left the picker.
      defaultPath: settings?.defaultLocalRoot || undefined
    });
    if (typeof picked === "string" && draft) draft.source = picked;
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (!draft) return;
    formError = "";
    const payload: Profile = {
      ...draft,
      excludes: excludesText
        .split("\n")
        .map((line) => line.trim())
        .filter(Boolean)
    };
    try {
      const saved = await api.saveProfile(payload);
      draft = null;
      await refresh();
      selectedId = saved.id;
    } catch (err) {
      formError = String(err);
    }
  }

  async function toggleEnabled(profile: Profile) {
    try {
      await api.setProfileEnabled(profile.id, !profile.enabled);
      await refresh();
    } catch (err) {
      setError(profile.id, err);
      await refresh();
    }
  }

  async function runNow(profile: Profile) {
    try {
      await api.syncNow(profile.id);
    } catch (err) {
      setError(profile.id, err);
    }
  }

  /// Relative is what you actually want at a glance; the absolute timestamp
  /// goes in the tooltip. `now` is a reactive tick so this refreshes on screen.
  function formatAgo(seconds: number | null | undefined): string {
    if (!seconds) return "never";
    const elapsed = Math.max(0, Math.round(now / 1000 - seconds));
    if (elapsed < 10) return "just now";
    if (elapsed < 60) return `${elapsed}s ago`;
    if (elapsed < 3600) return `${Math.floor(elapsed / 60)} min ago`;
    if (elapsed < 86_400) return `${Math.floor(elapsed / 3600)} h ago`;
    return `${Math.floor(elapsed / 86_400)} d ago`;
  }

  function formatExact(seconds: number | null | undefined): string {
    return seconds ? new Date(seconds * 1000).toLocaleString() : "never synced";
  }

  // Right-click menu, positioned at the cursor.
  let menu = $state<{ x: number; y: number; profile: Profile } | null>(null);
  let pendingDelete = $state<Profile | null>(null);

  function openMenu(event: MouseEvent, profile: Profile) {
    event.preventDefault();
    // Clamped so a right-click near the edge does not push the menu off-screen.
    menu = {
      x: Math.min(event.clientX, window.innerWidth - 170),
      y: Math.min(event.clientY, window.innerHeight - 130),
      profile
    };
  }

  /// Copy an existing profile into the editor rather than saving it outright:
  /// a duplicate shares the source and remote path, so it wants adjusting
  /// before it exists. It also arrives disabled, so saving it cannot start a
  /// second watcher on the same folder by surprise.
  function duplicate(profile: Profile) {
    draft = {
      ...profile,
      id: "",
      name: `${profile.name} (copy)`,
      enabled: false,
      excludes: [...profile.excludes]
    };
    excludesText = draft.excludes.join("\n");
    formError = "";
  }

  async function confirmDelete() {
    const profile = pendingDelete;
    pendingDelete = null;
    if (!profile) return;
    await api.deleteProfile(profile.id);
    delete logs[profile.id];
    delete progress[profile.id];
    await refresh();
  }

  let logEl = $state<HTMLPreElement | null>(null);
  let stickToBottom = $state(true);

  // Follow new output, but stop the moment the user scrolls up to read
  // something, and resume when they scroll back down.
  function onLogScroll() {
    if (!logEl) return;
    stickToBottom = logEl.scrollHeight - logEl.scrollTop - logEl.clientHeight < 40;
  }

  $effect(() => {
    selectedLog.length;
    if (logEl && stickToBottom) logEl.scrollTop = logEl.scrollHeight;
  });
</script>

<svelte:window
  onclick={() => (menu = null)}
  onresize={() => (menu = null)}
  onkeydown={(e) => {
    if (e.key === "Escape") menu = null;
  }}
/>

{#if menu}
  {@const target = menu.profile}
  <ul class="context-menu" style="left: {menu.x}px; top: {menu.y}px">
    <li><button onclick={() => startEdit(target)}>Edit</button></li>
    <li><button onclick={() => duplicate(target)}>Duplicate</button></li>
    <li><button class="danger" onclick={() => (pendingDelete = target)}>Delete…</button></li>
  </ul>
{/if}

{#if pendingDelete}
  <ConfirmDialog
    title="Delete “{pendingDelete.name}”?"
    message="The profile and its settings are removed. Files on your Mac and on the remote host are left untouched."
    confirmLabel="Delete profile"
    onconfirm={confirmDelete}
    oncancel={() => (pendingDelete = null)}
  />
{/if}

<div class="app">
  <header>
    <h1>SyncPoint</h1>
    {#if rsync}
      <span class="badge" class:warn={rsync.isOpenrsync} title={rsync.versionLine}>
        {rsync.isOpenrsync ? "openrsync (limited)" : `rsync ${rsync.major}.x`}
      </span>
    {:else}
      <span class="badge error">no rsync found</span>
    {/if}
    <span class="spacer"></span>
    <button class="cog" title="Settings (⌘,)" aria-label="Settings" onclick={() => api.openSettings()}>
      <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">
        <g
          fill="none"
          stroke="currentColor"
          stroke-width="1.4"
          stroke-linecap="round"
        >
          <circle cx="8" cy="8" r="4.6" />
          <circle cx="8" cy="8" r="1.9" />
          <path d="M12.6 8h1.7M11.25 11.25l1.2 1.2M8 12.6v1.7M4.75 11.25l-1.2 1.2
                   M3.4 8H1.7M4.75 4.75l-1.2-1.2M8 3.4V1.7M11.25 4.75l1.2-1.2" />
        </g>
      </svg>
    </button>
  </header>

  <div class="body">
    <aside>
      <button
        class="primary block"
        title={connections.length ? "New profile" : "Add a connection first"}
        disabled={connections.length === 0}
        onclick={startNew}>New profile</button
      >
      <ul class="items">
        {#each profiles as profile (profile.id)}
          <li>
            <button
              class="item"
              class:active={profile.id === selectedId}
              onclick={() => (selectedId = profile.id)}
              oncontextmenu={(e) => openMenu(e, profile)}
            >
              <span class="dot {stateOf(profile.id)}"></span>
              <span class="item-meta">
                <strong>{profile.name}</strong>
                <small title={formatExact(statuses[profile.id]?.lastSync)}>
                  {#if stateOf(profile.id) === "syncing" && progress[profile.id]}
                    Syncing {progress[profile.id].percent}%
                  {:else if stateOf(profile.id) === "idle"}
                    Synced {formatAgo(statuses[profile.id]?.lastSync)}
                  {:else}
                    {STATE_LABEL[stateOf(profile.id)]}
                  {/if}
                </small>
              </span>
            </button>
          </li>
        {/each}
        {#if profiles.length === 0}
          <li class="empty">No profiles yet.</li>
        {/if}
      </ul>
    </aside>

    <main>
      {#if draft}
        <form onsubmit={save}>
          <h2>{draft.id ? "Edit profile" : "New profile"}</h2>

          <label>
            Name
            <input bind:value={draft.name} placeholder="Project alpha" required autocapitalize="off" autocorrect="off" spellcheck="false" />
          </label>

          <label>
            Local source folder
            <span class="row">
              <input bind:value={draft.source} placeholder="/Users/you/project" required autocapitalize="off" autocorrect="off" spellcheck="false" />
              <button type="button" onclick={pickSource}>Browse…</button>
            </span>
          </label>

          <label>
            Connection
            <span class="row">
              <select class="grow" bind:value={draft.connectionId} required>
                {#each connections as connection (connection.id)}
                  <option value={connection.id}>
                    {connection.name} — {connectionTarget(connection)}
                  </option>
                {/each}
              </select>
              <button type="button" onclick={() => api.openSettings("connections")}>Manage…</button>
            </span>
          </label>

          <label>
            Remote path
            <input bind:value={draft.remotePath} placeholder="/srv/app or ~/app" required autocapitalize="off" autocorrect="off" spellcheck="false" />
          </label>

          <label>
            Excludes (one pattern per line)
            <textarea
              bind:value={excludesText}
              rows="4"
              autocapitalize="off"
              spellcheck="false"
              {...{ autocorrect: "off" }}
            ></textarea>
          </label>

          <label class="debounce">
            Debounce (ms)
            <input type="number" bind:value={draft.debounceMs} min="50" max="60000" />
            <small>Quiet period after the last change before rsync runs.</small>
          </label>

          <label class="check">
            <input type="checkbox" bind:checked={draft.delete} />
            <span>
              Mirror mode (<code>--delete</code>)
              <small>Files removed locally are also removed on the remote.</small>
            </span>
          </label>

          <label class="check">
            <input type="checkbox" bind:checked={draft.enabled} />
            <span>Watch this folder and sync automatically</span>
          </label>

          {#if formError}
            <p class="alert">{formError}</p>
          {/if}

          <span class="row end">
            <button type="button" onclick={() => (draft = null)}>Cancel</button>
            <button type="submit" class="primary">Save</button>
          </span>
        </form>
      {:else if selected}
        <div class="detail">
          <div class="detail-head">
            <div class="titles">
              <h2>{selected.name}</h2>
              <p class="path">{selected.source}</p>
              <p class="path">
                →
                {#if connectionOf(selected)}
                  {connectionTarget(connectionOf(selected)!)}:{selected.remotePath}
                {:else}
                  <em>no connection</em>
                {/if}
              </p>
            </div>
            <span class="chip {stateOf(selected.id)}">{STATE_LABEL[stateOf(selected.id)]}</span>
          </div>

          <div class="row">
            <button class="primary" onclick={() => runNow(selected!)}>Sync now</button>
            <button onclick={() => toggleEnabled(selected!)}>
              {selected.enabled ? "Disable" : "Enable"}
            </button>
            <button onclick={() => startEdit(selected!)}>Edit</button>
            <button class="danger" onclick={() => (pendingDelete = selected)}>Delete</button>
          </div>

          <dl class="facts">
            <div>
              <dt>Connection</dt>
              <dd>{connectionOf(selected)?.name ?? "— missing —"}</dd>
            </div>
            <div><dt>Mirror</dt><dd>{selected.delete ? "on (--delete)" : "off"}</dd></div>
            <div><dt>Debounce</dt><dd>{selected.debounceMs} ms</dd></div>
            <div>
              <dt>Last sync</dt>
              <dd title={formatExact(statuses[selected.id]?.lastSync)}>
                {formatAgo(statuses[selected.id]?.lastSync)}
              </dd>
            </div>
          </dl>

          {#if progress[selected.id]}
            {@const p = progress[selected.id]}
            <div class="progress">
              <div class="track"><div class="fill" style="width: {p.percent}%"></div></div>
              <small>
                {p.percent}% · {p.transferred}{p.rate ? ` · ${p.rate}` : ""}{p.eta
                  ? ` · ${p.eta} left`
                  : ""}
              </small>
            </div>
          {:else if stateOf(selected.id) === "syncing" && rsync && !rsync.supportsProgress}
            <p class="muted-line progress-note">
              Syncing — no percentage available, because {rsync.isOpenrsync
                ? "openrsync"
                : "this rsync"} lacks <code>--info=progress2</code>.
            </p>
          {/if}

          {#if statuses[selected.id]?.message}
            <p class="alert" class:ok={stateOf(selected.id) !== "error"}>
              {statuses[selected.id].message}
            </p>
          {/if}

          <h3>Output</h3>
          <pre
            class="log"
            bind:this={logEl}
            onscroll={onLogScroll}>{#each selectedLog as entry}<span class={entry.stream}
              >{entry.line}</span
            >{/each}{#if selectedLog.length === 0}<span class="muted">No output yet.</span>{/if}</pre>
        </div>
      {:else if connections.length === 0}
        <div class="placeholder">
          <div class="stack">
            <p>Add an SSH connection first, then create a profile that uses it.</p>
            <button class="primary" onclick={() => api.openSettings("connections")}>
              Add a connection
            </button>
          </div>
        </div>
      {:else}
        <div class="placeholder">
          <div class="stack">
            <p>Select a profile, or create one.</p>
            <button class="primary" onclick={startNew}>New profile</button>
          </div>
        </div>
      {/if}
    </main>
  </div>
</div>
