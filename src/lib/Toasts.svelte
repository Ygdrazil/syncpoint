<script lang="ts">
  import { fly } from "svelte/transition";
  import type { Toast } from "$lib/types";

  type Props = {
    toasts: Toast[];
    ondismiss: (id: number) => void;
  };

  let { toasts, ondismiss }: Props = $props();
</script>

<!-- aria-live rather than role="alert": these report what just happened and
     should not interrupt whatever is being read. -->
<div class="toasts" aria-live="polite">
  {#each toasts as toast (toast.id)}
    <div class="toast {toast.tone}" transition:fly={{ y: 12, duration: 160 }}>
      <span class="toast-text">{toast.text}</span>
      {#if toast.action}
        <button
          class="toast-action"
          onclick={toast.action.onclick}
        >{toast.action.label}</button>
      {/if}
      <button
        class="toast-close"
        title="Dismiss"
        aria-label="Dismiss"
        onclick={() => ondismiss(toast.id)}>×</button
      >
      {#if toast.seconds}
        <!-- Runs down over the toast's lifetime, so it is visibly about to go
             rather than vanishing without warning. -->
        <span class="toast-life" style="animation-duration: {toast.seconds}s"></span>
      {/if}
    </div>
  {/each}
</div>
