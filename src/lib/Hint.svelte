<script lang="ts">
  import type { Snippet } from "svelte";

  type Props = {
    /// What the tooltip explains, announced in place of the bare "?".
    label: string;
    children: Snippet;
  };

  let { label, children }: Props = $props();
  // Ties the badge to its bubble, so the text is not hover-only for anyone
  // reading with assistive tech.
  const id = $props.id();
</script>

<span class="hint">
  <button type="button" class="hint-badge" aria-label={label} aria-describedby={id}>?</button>
  <span class="hint-bubble" {id} role="tooltip">{@render children()}</span>
</span>
