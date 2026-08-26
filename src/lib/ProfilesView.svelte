<script lang="ts">
  import { onMount } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { open } from "@tauri-apps/plugin-dialog";
  import { check } from "@tauri-apps/plugin-updater";
  import * as api from "$lib/api";
  import ConfirmDialog from "$lib/ConfirmDialog.svelte";
  import PromptDialog from "$lib/PromptDialog.svelte";
  import Toasts from "$lib/Toasts.svelte";
  import {
    connectionTarget,
    emptyProfile,
    type Connection,
    type Folder,
    type LogBatch,
    type LogLine,
    type Profile,
    type RsyncInfo,
    type Settings,
    type SyncProgress,
    type SyncState,
    type SyncStatus,
    type Toast
  } from "$lib/types";

  const MAX_LOG_LINES = 500;

  let profiles = $state<Profile[]>([]);
  let connections = $state<Connection[]>([]);
  let folders = $state<Folder[]>([]);
  let statuses = $state<Record<string, SyncStatus>>({});
  let logs = $state<Record<string, LogLine[]>>({});
  let rsync = $state<RsyncInfo | null>(null);
  let settings = $state<Settings | null>(null);
  let progress = $state<Record<string, SyncProgress>>({});
  // Ticks so relative "last sync" labels do not go stale on screen.
  let now = $state(Date.now());

  let selectedId = $state<string | null>(null);
  /// Whether anything has been selected yet, so the opening draw can pick a
  /// profile without every later refresh doing the same.
  let hasSelected = false;
  let draft = $state<Profile | null>(null);
  let formError = $state("");
  let excludesText = $state("");

  let selected = $derived(profiles.find((p) => p.id === selectedId) ?? null);
  let selectedLog = $derived(selectedId ? (logs[selectedId] ?? []) : []);

  const STATE_LABEL: Record<SyncState, string> = {
    disabled: "Paused",
    idle: "Watching",
    pending: "Changes queued",
    syncing: "Syncing",
    error: "Error",
    // Distinct from a profile's own "Paused": this one lifts with Pause all,
    // and knowing which is which is how you tell what Resume all will start.
    paused: "All paused"
  };

  let paused = $derived(settings?.paused ?? false);

  /// Compact mode: the detail pane is hidden and the window narrows to the
  /// sidebar, leaving a list of profiles and their status. Held in settings so
  /// the app reopens the way it was left; the window resize itself happens in
  /// Rust, which also remembers the width to come back to.
  ///
  /// The editor has to be visible, so an open draft holds the pane open.
  /// Deriving that rather than turning the setting off means the pane lasts
  /// exactly as long as the edit and the window returns to compact when it
  /// ends — saved, cancelled or clicked away — with nothing to put back.
  let compact = $derived((settings?.compact ?? false) && draft === null);

  /// What the window has actually been told. Starting at `false` matches a
  /// freshly opened window, so an ordinary launch does not resize anything —
  /// only a saved `true`, or a later change, reaches Rust.
  let appliedCompact = false;

  $effect(() => {
    const wanted = compact;
    if (appliedCompact === wanted) return;
    appliedCompact = wanted;
    api.setCompact(wanted).catch((err) => notify(String(err), { tone: "error", sticky: true }));
  });

  let toasts = $state<Toast[]>([]);
  let nextToastId = 0;
  // The version an update toast offered, so the Update button can hand it off
  // to the About window (kept for any future use; the auto-update flag is what
  // actually drives About).
  let pendingUpdateVersion = $state("");

  /// How long a passing toast lasts. Errors and import summaries opt out with
  /// `sticky` and wait to be dismissed — they are worth reading twice.
  const TOAST_SECONDS = 5;

  function notify(
    text: string,
    options: { tone?: Toast["tone"]; sticky?: boolean; action?: Toast["action"] } = {}
  ) {
    const id = nextToastId++;
    const seconds = options.sticky ? undefined : TOAST_SECONDS;
    toasts.push({ id, text, tone: options.tone ?? "info", seconds, action: options.action });
    if (seconds) setTimeout(() => dismissToast(id), seconds * 1000);
  }

  function dismissToast(id: number) {
    const at = toasts.findIndex((t) => t.id === id);
    if (at >= 0) toasts.splice(at, 1);
  }

  async function syncAll() {
    const started = await api.syncAll();
    notify(
      started === 0 ? "Nothing to sync — no profile is enabled." : `Queued ${started} profile(s).`
    );
  }

  async function togglePaused() {
    await api.setPaused(!paused);
    await refresh();
  }

  let collapsed = $state<Record<string, boolean>>({});

  type Child =
    | { kind: "folder"; id: string; position: number; folder: Folder }
    | { kind: "profile"; id: string; position: number; profile: Profile };

  /// Everything directly inside a level, in the one order folders and profiles
  /// share. Mirrors `folder::children` — a folder can sit between two profiles,
  /// so a drop between any two rows means exactly what it looks like.
  function childrenOf(parentId: string | null): Child[] {
    const items: Child[] = [
      ...folders
        .filter((f) => (f.parentId ?? null) === parentId)
        .map((folder): Child => ({ kind: "folder", id: folder.id, position: folder.position, folder })),
      ...profiles
        .filter((p) => (p.folderId ?? null) === parentId)
        .map((profile): Child => ({
          kind: "profile",
          id: profile.id,
          position: profile.position,
          profile
        }))
    ];
    return items.sort((a, b) => a.position - b.position);
  }

  function parentOf(id: string): string | null {
    const folder = folders.find((f) => f.id === id);
    if (folder) return folder.parentId ?? null;
    return profiles.find((p) => p.id === id)?.folderId ?? null;
  }

  type Row =
    | { kind: "folder"; depth: number; folder: Folder; count: number }
    | { kind: "profile"; depth: number; profile: Profile };

  /// Flatten the tree with depth, skipping anything inside a collapsed folder.
  let rows = $derived.by(() => {
    const out: Row[] = [];

    const walk = (parentId: string | null, depth: number) => {
      // Bounded so a tree that is somehow cyclic cannot recurse forever here,
      // mirroring isSelfOrDescendant below.
      if (depth > folders.length) return;
      for (const child of childrenOf(parentId)) {
        if (child.kind === "profile") {
          out.push({ kind: "profile", depth, profile: child.profile });
          continue;
        }
        const inside = profilesInFolder(child.id).length;
        out.push({ kind: "folder", depth, folder: child.folder, count: inside });
        if (!collapsed[child.id]) walk(child.id, depth + 1);
      }
    };

    walk(null, 0);
    return out;
  });

  // ---------------------------------------------------------------------------
  // Drag and drop
  //
  // Driven by pointer events rather than HTML5 drag and drop, which does not
  // work here: wry overrides draggingEntered/draggingUpdated/performDragOperation
  // on the webview itself and Tauri's handler consumes the drag, so WebKit never
  // sees it. The escape hatch is dragDropEnabled: false, but that is per window
  // and would give up file drops onto the app for good.
  // ---------------------------------------------------------------------------

  /// Below this, the gesture is still a click. Without it, selecting a profile
  /// with a slightly unsteady hand would move it instead.
  const DRAG_THRESHOLD_PX = 4;

  type DragItem = { kind: "folder" | "profile"; id: string; label: string };

  /// Where the pointer currently resolves to, in the terms the backend takes
  /// plus what the list should draw. `row` is the row the marker attaches to,
  /// empty for the top-level zone.
  type Drop = {
    folderId: string | null;
    before: string | null;
    row: string;
    edge: "into" | "above" | "below";
  };

  let dragging = $state<DragItem | null>(null);
  let dragAt = $state({ x: 0, y: 0 });
  let drop = $state<Drop | null>(null);

  /// Where the press started, before it is known to be a drag at all.
  let pressed: { x: number; y: number; item: DragItem } | null = null;
  /// A drag ends with a pointerup over a row, which the browser follows with a
  /// click. Set so that click does not also select or collapse.
  let swallowClick = false;

  function press(event: PointerEvent, item: DragItem) {
    swallowClick = false;
    if (event.button !== 0) return;
    pressed = { x: event.clientX, y: event.clientY, item };
  }

  /// Runs the row's own action unless a drag just finished on top of it.
  function onClick(run: () => void) {
    if (swallowClick) {
      swallowClick = false;
      return;
    }
    run();
  }

  function onPointerMove(event: PointerEvent) {
    if (!pressed) return;
    if (!dragging) {
      const travelled = Math.hypot(event.clientX - pressed.x, event.clientY - pressed.y);
      if (travelled < DRAG_THRESHOLD_PX) return;
      dragging = pressed.item;
    }
    dragAt = { x: event.clientX, y: event.clientY };
    drop = resolveDrop(dragging, event.clientX, event.clientY);
  }

  async function onPointerUp() {
    const item = dragging;
    const target = drop;
    cancelDrag();
    if (!item) return;
    // Even a drag that landed nowhere must not fall through to a click.
    swallowClick = true;
    if (!target) return;

    try {
      await applyDrop(item, target);
    } catch (err) {
      notify(String(err), { tone: "error", sticky: true });
    }
  }

  function cancelDrag() {
    pressed = null;
    dragging = null;
    drop = null;
  }

  /// Mirrors `folder::is_self_or_descendant`. Gives up after as many steps as
  /// there are folders, so a tree that is somehow cyclic cannot spin here.
  function isSelfOrDescendant(candidate: string, ancestor: string): boolean {
    let current: string | null = candidate;
    for (let step = 0; step <= folders.length; step++) {
      if (current === null) return false;
      if (current === ancestor) return true;
      current = folders.find((f) => f.id === current)?.parentId ?? null;
    }
    return false;
  }

  let listEl = $state<HTMLUListElement | null>(null);

  /// A folder cannot land inside its own subtree. Only suppresses the marker
  /// so an impossible drop shows nothing; the backend still refuses it.
  function allowed(item: DragItem, target: Drop): Drop | null {
    if (item.kind !== "folder" || target.folderId === null) return target;
    return isSelfOrDescendant(target.folderId, item.id) ? null : target;
  }

  function resolveDrop(item: DragItem, x: number, y: number): Drop | null {
    if (!listEl) return null;
    const list = listEl.getBoundingClientRect();
    if (x < list.left || x > list.right || y < list.top || y > list.bottom) return null;

    // The first row still below the pointer. Resolved by geometry rather than
    // by hit testing the element under the cursor, so the gaps between rows
    // belong to the row beneath them instead of falling through to the list
    // and flashing the top-level marker on the way past.
    const all = [...listEl.querySelectorAll<HTMLElement>("[data-row-id]")];
    const row = all.find((el) => el.getBoundingClientRect().bottom > y);
    // Past the last row: the top level, at the end. The list keeps a strip of
    // padding below the rows so this stays reachable even when everything is
    // inside a folder.
    if (!row) return allowed(item, { folderId: null, before: null, row: "", edge: "into" });

    const id = row.dataset.rowId ?? "";
    const box = row.getBoundingClientRect();
    const offset = (y - box.top) / box.height;

    if (row.dataset.rowKind === "folder") {
      // An expanded folder has its children right below it, so a line under it
      // would point at the same gap as its first child. Only its top edge
      // orders it; everything else drops inside.
      const ordersBelow = collapsed[id] && offset > 0.72;
      if (offset > 0.28 && !ordersBelow) {
        return allowed(item, { folderId: id, before: null, row: id, edge: "into" });
      }
    }

    const parentId = parentOf(id);
    const siblings = childrenOf(parentId);
    const at = siblings.findIndex((c) => c.id === id);
    const above = offset < 0.5;

    return allowed(item, {
      folderId: parentId,
      before: above ? id : (siblings[at + 1]?.id ?? null),
      row: id,
      edge: above ? "above" : "below"
    });
  }

  async function applyDrop(item: DragItem, target: Drop) {
    // Landing on its own edge is where it already is; skip the write.
    if (target.before === item.id) return;
    await api.moveItem(item.kind, item.id, target.folderId, target.before);
    await refresh();
  }

  /// Drives PromptDialog. `prompt()` is unavailable in the webview, so naming
  /// a folder needs a real dialog rather than a browser one.
  let namePrompt = $state<{
    title: string;
    value: string;
    confirmLabel: string;
    apply: (name: string) => Promise<void>;
  } | null>(null);

  function newFolder(parentId: string | null = null) {
    namePrompt = {
      title: parentId ? "New folder inside" : "New folder",
      value: "",
      confirmLabel: "Create",
      apply: async (name) => {
        // The backend puts a new folder after everything already at its level.
        await api.saveFolder({ id: "", name, parentId, position: 0 });
        await refresh();
      }
    };
  }

  function renameFolder(folder: Folder) {
    namePrompt = {
      title: "Rename folder",
      value: folder.name,
      confirmLabel: "Rename",
      apply: async (name) => {
        await api.saveFolder({ ...folder, name });
        await refresh();
      }
    };
  }

  async function applyNamePrompt(name: string) {
    const pending = namePrompt;
    namePrompt = null;
    if (!pending) return;
    try {
      await pending.apply(name);
    } catch (err) {
      notify(String(err), { tone: "error", sticky: true });
    }
  }

  /// Everything a delete would take, counted through the whole subtree — the
  /// confirmation has to name what is about to be lost, not just the folder.
  function contentsOf(folderId: string): { profiles: number; folders: number } {
    let profiles = 0;
    let nested = 0;
    const walk = (id: string) => {
      profiles += profilesInFolder(id).length;
      for (const child of folders.filter((f) => (f.parentId ?? null) === id)) {
        nested += 1;
        walk(child.id);
      }
    };
    walk(folderId);
    return { profiles, folders: nested };
  }

  function profilesInFolder(folderId: string): Profile[] {
    return profiles.filter((p) => (p.folderId ?? null) === folderId);
  }

  function describeContents({ profiles, folders }: { profiles: number; folders: number }): string {
    const parts: string[] = [];
    if (profiles > 0) parts.push(`${profiles} profile${profiles === 1 ? "" : "s"}`);
    if (folders > 0) parts.push(`${folders} nested folder${folders === 1 ? "" : "s"}`);
    return parts.join(" and ");
  }

  async function removeFolder(folder: Folder) {
    const removed = await api.deleteFolder(folder.id);
    const lost = describeContents(removed);
    notify(
      lost
        ? `Deleted “${folder.name}” and everything in it — ${lost}.`
        : `Deleted “${folder.name}”.`
    );
    await refresh();
  }

  let folderMenu = $state<{ x: number; y: number; folder: Folder } | null>(null);
  let pendingFolderDelete = $state<Folder | null>(null);

  function stateOf(id: string): SyncState {
    return statuses[id]?.state ?? "disabled";
  }

  function connectionOf(profile: Profile): Connection | null {
    return connections.find((c) => c.id === profile.connectionId) ?? null;
  }

  async function refresh() {
    profiles = await api.listProfiles();
    connections = await api.listConnections();
    folders = await api.listFolders();
    settings = await api.getSettings();
    const list = await api.listStatuses();
    statuses = Object.fromEntries(list.map((s) => [s.profileId, s]));
    if (selectedId && !profiles.some((p) => p.id === selectedId)) selectedId = null;
    // Only pick one for the very first draw. After that an empty selection is
    // something the user asked for by clicking the list background, and any
    // refresh — a status arriving, another window saving — would undo it.
    if (!selectedId && !hasSelected && profiles.length > 0) selectedId = profiles[0].id;
    if (selectedId) hasSelected = true;
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
      listen("config-changed", () => refresh()),
      // Sent by the Finder Quick Action via syncpoint://new-profile.
      listen<string>("new-profile-from", (event) => {
        startNew();
        if (draft) draft.source = event.payload;
      })
    ];

    api.getRsyncInfo().then((info) => (rsync = info));
    refresh();

    const tick = setInterval(() => (now = Date.now()), 30_000);
    return () => {
      clearInterval(tick);
      unlisteners.forEach((u) => u.then((fn) => fn()));
    };
  });

  // Check for updates on launch — silent, no toast if up to date or error.
  (async () => {
    try {
      const update = await check();
      if (update) {
        const version = update.version;
        try { await update.close(); } catch {}
        pendingUpdateVersion = version;
        const toastId = nextToastId;
        notify(`SyncPoint v${version} available`, {
          sticky: true,
          action: {
            label: "Update",
            onclick: () => {
              api.openAbout(true);
              dismissToast(toastId);
            }
          }
        });
      }
    } catch {
      // Silent failure — don't bother the user on launch.
    }
  })();

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

  /// Creating things lives in one menu, reachable from the + in the head and
  /// from a right-click on empty list space. Two permanent full-width buttons
  /// gave the rarest actions the loudest place in the sidebar.
  let createMenu = $state<{ x: number; y: number } | null>(null);

  /// Keep a menu on screen when it is opened near an edge. Sized from the item
  /// count rather than a constant, which was quietly wrong the moment a menu
  /// grew a row.
  function placeMenu(x: number, y: number, items: number) {
    return {
      x: Math.min(x, window.innerWidth - 180),
      y: Math.min(y, window.innerHeight - (items * 30 + 12))
    };
  }

  function openCreateMenu(x: number, y: number) {
    createMenu = placeMenu(x, y, 2);
  }

  /// Click the list's own background, not a row: drop the selection, and with
  /// it whatever the detail pane was showing. An edit in progress goes too —
  /// the same as pressing Cancel.
  function clearSelection(event: MouseEvent) {
    if ((event.target as HTMLElement).closest("[data-row-id]")) return;
    if (swallowClick) {
      swallowClick = false;
      return;
    }
    selectedId = null;
    draft = null;
    formError = "";
  }

  /// Right-click anywhere in the list that is not a row.
  function onListContextMenu(event: MouseEvent) {
    event.preventDefault();
    openCreateMenu(event.clientX, event.clientY);
  }

  function openMenu(event: MouseEvent, profile: Profile) {
    event.preventDefault();
    // Otherwise this also reaches the list behind it and opens the create menu.
    event.stopPropagation();
    menu = { ...placeMenu(event.clientX, event.clientY, 6), profile };
  }

  /// Copy an existing profile into the editor rather than saving it outright:
  /// a duplicate shares the source and remote path, so it wants adjusting
  /// before it exists. It also arrives paused, so saving it cannot start a
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
  onclick={() => {
    menu = null;
    folderMenu = null;
    createMenu = null;
  }}
  onresize={() => (menu = null)}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={cancelDrag}
  onkeydown={(e) => {
    if (e.key === "Escape") {
      menu = null;
      folderMenu = null;
      createMenu = null;
      cancelDrag();
    }
  }}
/>

{#if dragging}
  <div class="drag-ghost" style="left: {dragAt.x + 14}px; top: {dragAt.y + 10}px">
    {dragging.label}
  </div>
{/if}

{#if menu}
  {@const target = menu.profile}
  <ul class="context-menu" style="left: {menu.x}px; top: {menu.y}px">
    <!-- Grouped by what they do to the profile: run it, change it, destroy it. -->
    <li><button onclick={() => runNow(target)}>Sync now</button></li>
    <li>
      <button onclick={() => toggleEnabled(target)}>
        {target.enabled ? "Pause" : "Resume"}
      </button>
    </li>
    <li class="sep" aria-hidden="true"></li>
    <li><button onclick={() => startEdit(target)}>Edit</button></li>
    <li><button onclick={() => duplicate(target)}>Duplicate</button></li>
    <li class="sep" aria-hidden="true"></li>
    <li><button class="danger" onclick={() => (pendingDelete = target)}>Delete…</button></li>
  </ul>
{/if}

{#if createMenu}
  <ul class="context-menu" style="left: {createMenu.x}px; top: {createMenu.y}px">
    <li>
      {#if connections.length > 0}
        <button onclick={startNew}>New profile</button>
      {:else}
        <!-- A disabled item naming the fix without offering it is worse than
             sending them where the fix is. -->
        <button onclick={() => api.openSettings("connections")}>Add a connection…</button>
      {/if}
    </li>
    <li><button onclick={() => newFolder(null)}>New folder</button></li>
  </ul>
{/if}

{#if folderMenu}
  {@const target = folderMenu.folder}
  <ul class="context-menu" style="left: {folderMenu.x}px; top: {folderMenu.y}px">
    <li><button onclick={() => renameFolder(target)}>Rename…</button></li>
    <li><button onclick={() => newFolder(target.id)}>New folder inside</button></li>
    <li>
      <button class="danger" onclick={() => (pendingFolderDelete = target)}>Delete…</button>
    </li>
  </ul>
{/if}

{#if namePrompt}
  <PromptDialog
    title={namePrompt.title}
    label="Folder name"
    value={namePrompt.value}
    placeholder="Client projects"
    confirmLabel={namePrompt.confirmLabel}
    onconfirm={applyNamePrompt}
    oncancel={() => (namePrompt = null)}
  />
{/if}

{#if pendingFolderDelete}
  {@const inside = describeContents(contentsOf(pendingFolderDelete.id))}
  <ConfirmDialog
    title="Delete folder “{pendingFolderDelete.name}”?"
    message={inside
      ? `This also deletes everything inside it — ${inside}. Files on your Mac and on the remote host are left untouched, and nothing already synced is removed from the remote.`
      : "The folder is empty. Nothing else is removed."}
    confirmLabel={inside ? "Delete folder and contents" : "Delete folder"}
    onconfirm={() => {
      const target = pendingFolderDelete;
      pendingFolderDelete = null;
      if (target) removeFolder(target);
    }}
    oncancel={() => (pendingFolderDelete = null)}
  />
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

<Toasts {toasts} ondismiss={dismissToast} />

<div class="app" class:compact>
  <aside>
    <div class="side-head" data-tauri-drag-region>
      <div class="side-title" data-tauri-drag-region>
        <h1 data-tauri-drag-region>SyncPoint</h1>
        <span class="spacer" data-tauri-drag-region></span>
        <button
          class="cog"
          title="New profile or folder"
          aria-label="New profile or folder"
          onclick={(e) => {
            // The window listener closes every menu, so this click must not
            // reach it or the menu would shut as it opened.
            e.stopPropagation();
            const box = e.currentTarget.getBoundingClientRect();
            if (createMenu) createMenu = null;
            else openCreateMenu(box.left, box.bottom + 4);
          }}
        >
          <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">
            <path
              d="M8 3.4v9.2M3.4 8h9.2"
              fill="none"
              stroke="currentColor"
              stroke-width="1.6"
              stroke-linecap="round"
            />
          </svg>
        </button>
        <button
          class="cog"
          title="Settings (⌘,)"
          aria-label="Settings"
          onclick={() => api.openSettings()}
        >
          <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">
            <g fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round">
              <circle cx="8" cy="8" r="4.6" />
              <circle cx="8" cy="8" r="1.9" />
              <path d="M12.6 8h1.7M11.25 11.25l1.2 1.2M8 12.6v1.7M4.75 11.25l-1.2 1.2
                       M3.4 8H1.7M4.75 4.75l-1.2-1.2M8 3.4V1.7M11.25 4.75l1.2-1.2" />
            </g>
          </svg>
        </button>
      </div>
      {#if rsync}
        <span class="badge" class:warn={rsync.isOpenrsync} title={rsync.versionLine}>
          {rsync.isOpenrsync ? "openrsync (limited)" : `rsync ${rsync.major}.x`}
        </span>
      {:else}
        <span class="badge error">no rsync found</span>
      {/if}
    </div>

    <!-- Background clicks: right-click creates, left-click deselects. Neither
         is a control with a role that fits, and neither is the only way to do
         its job — the + in the head creates, and the editor has a Cancel
         button — so nothing here is reachable by pointer alone. -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div class="side-scroll" onclick={clearSelection} oncontextmenu={onListContextMenu}>
      <ul class="items" class:drop-root={drop?.row === ""} bind:this={listEl}>
        {#each rows as row (row.kind === "folder" ? row.folder.id : row.profile.id)}
          {@const id = row.kind === "folder" ? row.folder.id : row.profile.id}
          <li
            class="list-row"
            class:dragging={dragging?.id === id}
            class:drop-into={drop?.row === id && drop.edge === "into"}
            class:drop-above={drop?.row === id && drop.edge === "above"}
            class:drop-below={drop?.row === id && drop.edge === "below"}
            style="padding-left: {row.depth * 0.85}rem"
            data-row-id={id}
            data-row-kind={row.kind}
            onpointerdown={(e) =>
              press(e, {
                kind: row.kind,
                id,
                label: row.kind === "folder" ? row.folder.name : row.profile.name
              })}
          >
            {#if row.kind === "folder"}
              <button
                class="item folder"
                onclick={() => onClick(() => (collapsed[row.folder.id] = !collapsed[row.folder.id]))}
                oncontextmenu={(e) => {
                  e.preventDefault();
                  e.stopPropagation();
                  folderMenu = {
                    ...placeMenu(e.clientX, e.clientY, 3),
                    folder: row.folder
                  };
                }}
              >
                <svg class="folder-icon" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">
                  <g transform="scale(0.667)">
                    {#if collapsed[row.folder.id]}
                      <path
                        d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"
                        fill="none"
                        stroke="currentColor"
                        stroke-width="2"
                        stroke-linecap="round"
                        stroke-linejoin="round"
                      />
                    {:else}
                      <path
                        d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2"
                        fill="none"
                        stroke="currentColor"
                        stroke-width="2"
                        stroke-linecap="round"
                        stroke-linejoin="round"
                      />
                    {/if}
                  </g>
                </svg>
                <strong>{row.folder.name}</strong>
                {#if row.count > 0}
                  <span class="folder-count" title="{row.count} profile{row.count === 1 ? '' : 's'}">
                    {row.count}
                  </span>
                {/if}
              </button>
            {:else}
              <button
                class="item"
                class:active={row.profile.id === selectedId}
                onclick={() => onClick(() => (selectedId = row.profile.id))}
                oncontextmenu={(e) => openMenu(e, row.profile)}
              >
                <span class="dot {stateOf(row.profile.id)}"></span>
                <span class="item-meta">
                  <strong>{row.profile.name}</strong>
                  <small title={formatExact(statuses[row.profile.id]?.lastSync)}>
                    {#if stateOf(row.profile.id) === "syncing" && progress[row.profile.id]}
                      Syncing {progress[row.profile.id].percent}%
                    {:else if stateOf(row.profile.id) === "idle"}
                      Synced {formatAgo(statuses[row.profile.id]?.lastSync)}
                    {:else}
                      {STATE_LABEL[stateOf(row.profile.id)]}
                    {/if}
                  </small>
                </span>
              </button>
            {/if}
          </li>
        {/each}
        {#if profiles.length === 0 && folders.length === 0}
          <li class="empty">No profiles yet — use + above, or right-click here.</li>
        {/if}
      </ul>
      </div>

      <div class="side-foot">
        {#if profiles.some((p) => p.enabled)}
          <button title="Sync every enabled profile now" onclick={syncAll}>Sync all</button>
        {/if}
        {#if profiles.length > 0}
          <button
            class:on={paused}
            title={paused
              ? "Resume all profiles"
              : "Suspend all profiles without changing which are enabled"}
            onclick={togglePaused}
          >
            {paused ? "Resume all" : "Pause all"}
          </button>
        {/if}
      </div>
    </aside>

    <main>
      <div class="main-head" data-tauri-drag-region></div>

      <div class="pane">
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
              Delete files on the remote when they are deleted locally
              <small>
                The remote becomes an exact copy: anything this profile does not
                send is removed there, excluded files
                {draft.excludes.length > 0
                  ? `(${draft.excludes.slice(0, 3).join(", ")})`
                  : ""} included. Without that, rsync protects excluded files
                from deletion and a folder still holding one cannot be removed
                remotely at all.
              </small>
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
              {selected.enabled ? "Pause" : "Resume"}
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
      </div>
    </main>
</div>
