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
      <div class="toast-row">
        {#if toast.action}
          <!-- Only ever set for the update-available toast today — an icon
               here is what tells it apart from a routine status message. -->
          <svg class="toast-icon" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">
            <g transform="scale(0.667)">
              <circle cx="12" cy="12" r="10" fill="none" stroke="currentColor" stroke-width="2" />
              <path
                d="m16 12-4-4-4 4M12 16V8"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </g>
          </svg>
        {/if}
        <span class="toast-text">{toast.text}</span>
        <button
          class="toast-close"
          title="Dismiss"
          aria-label="Dismiss"
          onclick={() => ondismiss(toast.id)}>×</button
        >
      </div>
      {#if toast.action}
        <div class="toast-actions">
          <button class="toast-action" onclick={toast.action.onclick}>{toast.action.label}</button>
        </div>
      {/if}
      {#if toast.seconds}
        <!-- Runs down over the toast's lifetime, so it is visibly about to go
             rather than vanishing without warning. -->
        <span class="toast-life" style="animation-duration: {toast.seconds}s"></span>
      {/if}
    </div>
  {/each}
</div>
