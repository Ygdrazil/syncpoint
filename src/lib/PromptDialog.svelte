<script lang="ts">
  // The webview has no working `prompt()`: wry's WKUIDelegate does not
  // implement runJavaScriptTextInputPanel, so WKWebView returns null without
  // ever showing anything. Native <dialog> gives the focus trap and Escape
  // handling for free.
  type Props = {
    title: string;
    label?: string;
    value?: string;
    placeholder?: string;
    confirmLabel?: string;
    onconfirm: (value: string) => void;
    oncancel: () => void;
  };

  let {
    title,
    label = "Name",
    value = "",
    placeholder = "",
    confirmLabel = "Save",
    onconfirm,
    oncancel
  }: Props = $props();

  let element = $state<HTMLDialogElement | null>(null);
  let input = $state<HTMLInputElement | null>(null);
  // Capturing the initial value is the intent: the dialog is mounted fresh for
  // each prompt, so there is no later prop change to track.
  // svelte-ignore state_referenced_locally
  let current = $state(value);

  $effect(() => {
    element?.showModal();
    // Select the existing text so a rename can be typed straight over.
    input?.select();
  });

  function submit(event: Event) {
    event.preventDefault();
    const trimmed = current.trim();
    if (trimmed) onconfirm(trimmed);
  }
</script>

<dialog bind:this={element} onclose={oncancel}>
  <form onsubmit={submit}>
    <h2>{title}</h2>
    <label>
      {label}
      <!-- svelte-ignore a11y_autofocus -->
      <input
        bind:this={input}
        bind:value={current}
        {placeholder}
        autofocus
        autocapitalize="off"
        autocorrect="off"
        spellcheck="false"
      />
    </label>
    <div class="row end">
      <button type="button" onclick={oncancel}>Cancel</button>
      <button type="submit" class="primary" disabled={!current.trim()}>{confirmLabel}</button>
    </div>
  </form>
</dialog>

<style>
  dialog {
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--panel);
    color: var(--text);
    padding: 1.1rem 1.25rem;
    min-width: 20rem;
    box-shadow: 0 12px 40px rgb(0 0 0 / 0.28);
  }

  dialog::backdrop {
    background: rgb(0 0 0 / 0.35);
  }

  h2 {
    margin: 0 0 0.75rem;
    font-size: 1rem;
  }

  form {
    display: flex;
    flex-direction: column;
    gap: 0.85rem;
  }
</style>
