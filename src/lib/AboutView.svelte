<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "$lib/api";
  import type { AppInfo } from "$lib/types";

  let info = $state<AppInfo | null>(null);
  let copied = $state(false);

  onMount(() => {
    api.getAppInfo().then((value) => (info = value));
  });

  function rsyncLabel(value: AppInfo): string {
    if (!value.rsync) return "not found";
    return value.rsync.isOpenrsync
      ? `openrsync (limited) — ${value.rsync.path}`
      : `rsync ${value.rsync.major}.x — ${value.rsync.path}`;
  }

  // Everything worth pasting into a bug report, in one click.
  async function copyDetails() {
    if (!info) return;
    const text = [
      `${info.name} ${info.version}`,
      `rsync: ${rsyncLabel(info)}`,
      `config: ${info.configPath}`
    ].join("\n");
    await navigator.clipboard.writeText(text);
    copied = true;
    setTimeout(() => (copied = false), 1500);
  }
</script>

<div class="about">
  {#if info}
    <h1>{info.name}</h1>
    <p class="version">
      {info.version}
      {#if info.isDev}
        <span class="badge warn">development build</span>
      {/if}
    </p>

    <dl class="details">
      <dt>rsync</dt>
      <dd>{rsyncLabel(info)}</dd>
      <dt>Config</dt>
      <dd class="wrap">{info.configPath}</dd>
    </dl>

    <button onclick={copyDetails}>{copied ? "Copied" : "Copy details"}</button>

    <p class="fine">
      Mirrors local folders to remote hosts over SSH, driven by a filesystem
      watcher.
    </p>
  {:else}
    <p class="fine">Loading…</p>
  {/if}
</div>

<style>
  .about {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    gap: 0.6rem;
    padding: 2rem 1.5rem;
    box-sizing: border-box;
    min-height: 100vh;
  }

  h1 {
    margin: 0;
    font-size: 1.35rem;
  }

  .version {
    margin: 0;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.9rem;
    color: var(--muted);
  }

  .details {
    width: 100%;
    margin: 0.8rem 0 0;
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 0.35rem 0.8rem;
    text-align: left;
    font-size: 0.78rem;
  }
  .details dt {
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    font-size: 0.68rem;
    padding-top: 0.1rem;
  }
  .details dd {
    margin: 0;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  }
  .details dd.wrap {
    word-break: break-all;
  }

  .fine {
    margin: 0.4rem 0 0;
    font-size: 0.75rem;
    color: var(--muted);
    line-height: 1.5;
  }
</style>
