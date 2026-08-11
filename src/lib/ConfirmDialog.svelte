<script lang="ts">
  // Native <dialog> rather than a hand-rolled overlay: it brings the focus
  // trap, Escape handling and inertness of the page behind it for free.
  type Props = {
    title: string;
    message?: string;
    detail?: string;
    confirmLabel?: string;
    onconfirm: () => void;
    oncancel: () => void;
  };

  let {
    title,
    message = "",
    detail = "",
    confirmLabel = "Delete",
    onconfirm,
    oncancel
  }: Props = $props();

  let element = $state<HTMLDialogElement | null>(null);

  $effect(() => {
    element?.showModal();
  });
</script>

<dialog bind:this={element} onclose={oncancel} onclick={(e) => e.stopPropagation()}>
  <h2>{title}</h2>
  {#if message}<p>{message}</p>{/if}
  {#if detail}<pre class="detail">{detail}</pre>{/if}
  <div class="row end">
    <button type="button" onclick={oncancel}>Cancel</button>
    <button type="button" class="danger-solid" onclick={onconfirm}>{confirmLabel}</button>
  </div>
</dialog>

<style>
  dialog {
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--panel);
    color: var(--text);
    padding: 1.1rem 1.25rem;
    max-width: 26rem;
    box-shadow: 0 12px 40px rgb(0 0 0 / 0.28);
  }

  dialog::backdrop {
    background: rgb(0 0 0 / 0.35);
  }

  h2 {
    margin: 0 0 0.5rem;
    font-size: 1rem;
  }

  p {
    margin: 0 0 0.6rem;
    font-size: 0.85rem;
    color: var(--muted);
    line-height: 1.5;
  }

  .detail {
    margin: 0 0 0.9rem;
    padding: 0.5rem 0.6rem;
    max-height: 9rem;
    overflow: auto;
    border-radius: 6px;
    background: var(--bg);
    border: 1px solid var(--border);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.74rem;
    white-space: pre-wrap;
  }
</style>
