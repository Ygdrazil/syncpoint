<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "$lib/api";
  import Hint from "$lib/Hint.svelte";
  import type { RenderedPaths, Settings } from "$lib/types";

  import { open } from "@tauri-apps/plugin-dialog";

  /// Written out here, in braces Svelte would otherwise try to evaluate.
  const VALUES: [string, string][] = [
    ["{name}", "the profile name as typed"],
    ["{slug}", "the name made safe for a path"],
    ["{folder}", "its sidebar folder"],
    ["{folder_path}", "that folder and its parents"],
    ["{connection}", "the connection's name"],
    ["{user}", "the SSH user"],
    ["{host}", "the SSH host"],
    ["{home}", "your home folder on this Mac"],
    ["{machine}", "this Mac's name"]
  ];
  const SOURCE_VALUES: [string, string][] = [
    ["{source_name}", "the last part of the source path"],
    ["{source_parent}", "the folder above it"],
    ["{source_rel}", "the source path, from the folder to browse from"]
  ];

  let settings = $state<Settings | null>(null);
  let sourceTemplate = $state("");
  let targetTemplate = $state("");
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
    sourceTemplate = value.sourceTemplate;
    targetTemplate = value.targetTemplate;
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
      ? sourceTemplate.trim() !== settings.sourceTemplate ||
        targetTemplate.trim() !== settings.targetTemplate ||
        localRoot.trim() !== settings.defaultLocalRoot ||
        debounce !== settings.defaultDebounceMs ||
        parse(excludesText).join("\n") !== settings.defaultExcludes.join("\n")
      : false
  );

  // Shown so the effect of a template is obvious before saving.
  const NOTHING = "(empty — nothing prefilled)";
  let preview = $state<RenderedPaths>({ source: "", target: "" });
  let previewRequest = 0;
  $effect(() => {
    const request = ++previewRequest;
    api
      .previewPathTemplates(sourceTemplate, targetTemplate, localRoot)
      .then((paths) => {
        if (request === previewRequest) preview = paths;
      })
      .catch(() => {});
  });

  // With nothing in it to tell profiles apart, they would all get one destination.
  let targetIsFixed = $derived(
    targetTemplate.trim() !== "" && !/\{[a-z_]+\}/.test(targetTemplate)
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
          sourceTemplate: sourceTemplate.trim(),
          targetTemplate: targetTemplate.trim(),
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
      Source path template
      <Hint label="About the source path template">
        Prefilled into the source path of a new profile. Must start with
        <code>/</code>, <code>~/</code> or <code>{"{home}"}</code>; leave empty for
        no prefill.<br /><br />
        {#each VALUES as [value, meaning] (value)}
          <code>{value}</code> {meaning}<br />
        {/each}
      </Hint>
    </span>
    <input
      bind:value={sourceTemplate}
      placeholder={"~/dev/{slug}"}
      autocapitalize="off"
      autocorrect="off"
      spellcheck="false"
    />
    <small>Example: <code>{preview.source || NOTHING}</code></small>
  </div>

  <div class="setting">
    <span class="title">
      Remote path template
      <Hint label="About the remote path template">
        Prefilled into the remote path of a new profile. Must start with
        <code>/</code>, <code>~/</code> or <code>{"{home}"}</code>; leave empty for
        no prefill.<br /><br />
        {#each [...VALUES, ...SOURCE_VALUES] as [value, meaning] (value)}
          <code>{value}</code> {meaning}<br />
        {/each}
      </Hint>
    </span>
    <input
      bind:value={targetTemplate}
      placeholder={"/home/you/dev/{slug}"}
      autocapitalize="off"
      autocorrect="off"
      spellcheck="false"
    />
    <small>Example: <code>{preview.target || NOTHING}</code></small>
    {#if targetIsFixed}
      <p class="alert">
        Nothing here tells profiles apart, so every new one gets this same path.
        With delete on, they would wipe each other's files.
      </p>
    {/if}
  </div>

  <div class="setting">
    <span class="title">
      Local folder to browse from
      <Hint label="About the folder to browse from">
        Where the folder picker opens when choosing a profile's source, and what
        <code>{"{source_rel}"}</code> is measured from.
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
