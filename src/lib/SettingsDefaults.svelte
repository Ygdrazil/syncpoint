<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "$lib/api";
  import Hint from "$lib/Hint.svelte";
  import type { Settings } from "$lib/types";

  import { open } from "@tauri-apps/plugin-dialog";

  let settings = $state<Settings | null>(null);
  let prefix = $state("");
  let localRoot = $state("");
  let debounce = $state(500);
  let excludesText = $state("");
  let error = $state("");
  let saved = $state(false);

  function parse(value: string): string[] {
    return value
      .split("\n")
      .map((line) => line.trim())
      .filter(Boolean);
  }

  function load(value: Settings) {
    settings = value;
    prefix = value.defaultRemotePrefix;
    localRoot = value.defaultLocalRoot;
    debounce = value.defaultDebounceMs;
    excludesText = value.defaultExcludes.join("\n");
  }

  async function pickLocalRoot() {
    const picked = await open({
      directory: true,
      multiple: false,
      defaultPath: localRoot || undefined
    });
    if (typeof picked === "string") localRoot = picked;
  }

  let dirty = $derived(
    settings
      ? prefix.trim() !== settings.defaultRemotePrefix ||
        localRoot.trim() !== settings.defaultLocalRoot ||
        debounce !== settings.defaultDebounceMs ||
        parse(excludesText).join("\n") !== settings.defaultExcludes.join("\n")
      : false
  );

  // Shown so the effect of the prefix is obvious before saving.
  let preview = $derived(
    prefix.trim() ? `${prefix.trim().replace(/\/+$/, "")}/` : "(empty — nothing prefilled)"
  );

  onMount(() => {
    api.getSettings().then(load);
  });

  async function save() {
    if (!settings) return;
    error = "";
    try {
      load(
        await api.saveSettings({
          ...settings,
          defaultRemotePrefix: prefix.trim(),
          defaultLocalRoot: localRoot.trim(),
          defaultDebounceMs: debounce,
          defaultExcludes: parse(excludesText)
        })
      );
      saved = true;
      setTimeout(() => (saved = false), 1500);
    } catch (err) {
      error = String(err);
    }
  }

  function reset() {
    if (settings) load(settings);
  }
</script>

<h2>Profile defaults</h2>
<p class="muted-line">
  Applied when a profile is created. Changing them leaves existing profiles
  alone, so each stays tunable on its own.
</p>

{#if settings}
  <div class="setting">
    <span class="title">
      Remote path prefix
      <Hint label="About the remote path prefix">
        Prefilled into the remote path of a new profile, so you only type the
        last part. Must start with <code>/</code> or <code>~/</code>; leave empty
        for no prefill.
      </Hint>
    </span>
    <input
      bind:value={prefix}
      placeholder="/srv/apps"
      autocapitalize="off"
      autocorrect="off"
      spellcheck="false"
    />
    <small>New profiles start with: <code>{preview}</code></small>
  </div>

  <div class="setting">
    <span class="title">
      Local folder to browse from
      <Hint label="About the folder to browse from">
        Where the folder picker opens when choosing a profile's source. It is not
        prefilled into the field — a partial local path would fail validation
        straight away, since the source has to be a folder that exists.
      </Hint>
    </span>
    <span class="row">
      <input
        bind:value={localRoot}
        placeholder="/Users/you/dev"
        autocapitalize="off"
        autocorrect="off"
        spellcheck="false"
      />
      <button type="button" onclick={pickLocalRoot}>Browse…</button>
    </span>
  </div>

  <div class="setting">
    <span class="title">
      Debounce
      <Hint label="About debounce">
        Quiet period after the last change before rsync runs. Longer coalesces
        more edits into one sync; shorter reacts faster.
      </Hint>
    </span>
    <input class="narrow" type="number" bind:value={debounce} min="50" max="60000" />
  </div>

  <div class="setting">
    <span class="title">
      Excludes
      <Hint label="About exclude patterns">
        One per line, passed straight to <code>rsync --exclude</code>. A bare
        name like <code>.git</code> matches at any depth;
        <code>target/debug</code> is anchored to the top of the folder;
        <code>*.log</code> matches by suffix.
      </Hint>
    </span>
    <textarea
      bind:value={excludesText}
      rows="8"
      autocapitalize="off"
      spellcheck="false"
      {...{ autocorrect: "off" }}
    ></textarea>
  </div>

  {#if error}
    <p class="alert">{error}</p>
  {/if}

  <span class="row end">
    {#if saved}<small class="muted-line">Saved</small>{/if}
    <button onclick={reset} disabled={!dirty}>Revert</button>
    <button class="primary" onclick={save} disabled={!dirty}>Save</button>
  </span>
{:else}
  <p class="muted-line">Loading…</p>
{/if}
