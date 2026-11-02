<script lang="ts">
  import type { WhatsNew } from "$lib/types";

  type Props = {
    whatsNew: WhatsNew;
    /// However it was closed, Escape included.
    onclosed: (neverAgain: boolean) => void;
  };

  let { whatsNew, onclosed }: Props = $props();

  const LABEL = { added: "Added", changed: "Changed", fixed: "Fixed" };

  let element = $state<HTMLDialogElement | null>(null);
  let closeButton = $state<HTMLButtonElement | null>(null);
  let neverAgain = $state(false);

  $effect(() => {
    element?.showModal();
    // Otherwise focus lands on the checkbox, one key press from opting out.
    closeButton?.focus();
  });
</script>

<dialog
  bind:this={element}
  onclose={() => onclosed(neverAgain)}
  onclick={(e) => e.stopPropagation()}
>
  <h2>What's new ?</h2>
  <p class="version">Version {whatsNew.version}</p>

  <ul>
    {#each whatsNew.changes as change, index (index)}
      <li>
        <span class="tag {change.kind}">{LABEL[change.kind]}</span>
        <span>{change.text}</span>
      </li>
    {/each}
  </ul>

  <div class="foot">
    <label>
      <input type="checkbox" bind:checked={neverAgain} />
      Never show me this again
    </label>
    <button
      bind:this={closeButton}
      type="button"
      class="primary"
      onclick={() => element?.close()}>Close</button
    >
  </div>
</dialog>

<style>
  dialog {
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--panel);
    color: var(--text);
    padding: 1.1rem 1.25rem;
    /* Compact mode narrows the window to the sidebar. */
    width: min(28rem, calc(100vw - 2rem));
    box-sizing: border-box;
    box-shadow: 0 12px 40px rgb(0 0 0 / 0.28);
  }

  dialog::backdrop {
    background: rgb(0 0 0 / 0.35);
  }

  h2 {
    margin: 0;
    font-size: 1.05rem;
  }

  .version {
    margin: 0.1rem 0 0.8rem;
    font-size: 0.78rem;
    color: var(--muted);
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    /* Skipping several versions can make this long. */
    max-height: 50vh;
    overflow-y: auto;
  }

  li {
    display: grid;
    grid-template-columns: 3.7rem minmax(0, 1fr);
    gap: 0.5rem;
    align-items: baseline;
    margin: 0.4rem 0;
    font-size: 0.85rem;
    line-height: 1.4;
  }

  .tag {
    font-size: 0.68rem;
    font-weight: 600;
    border-radius: 4px;
    padding: 0.05rem 0.35rem;
    text-align: center;
  }

  .tag.added {
    color: var(--ok);
    background: color-mix(in srgb, var(--ok) 15%, transparent);
  }

  .tag.changed {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 15%, transparent);
  }

  .tag.fixed {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 15%, transparent);
  }

  .foot {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.75rem;
    margin-top: 1rem;
  }

  label {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.78rem;
    color: var(--muted);
  }
</style>
