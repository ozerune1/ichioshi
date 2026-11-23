<script lang="ts">
  // One of a few, as chips (from the view: label, on, handle).
  import { press } from "./api";
  let { options, scroll = false }: { options: { label: string; on: boolean; id: string }[]; scroll?: boolean } = $props();
</script>

<div class="chips" class:scroll>
  {#each options as o}
    <button class="chip" class:on={o.on} onclick={() => press(o.id)}>{o.label}</button>
  {/each}
</div>

<style>
  .chips { display: flex; flex-wrap: wrap; gap: 8px; }
  .chips.scroll { flex-wrap: nowrap; overflow-x: auto; margin: 0 -16px; padding: 0 16px 2px; scrollbar-width: none; }
  .chip {
    flex: none; font: inherit; font-size: 13px; font-weight: 500; padding: 8px 14px; border-radius: 999px;
    border: 1px solid var(--line); background: var(--surface); color: var(--muted); white-space: nowrap;
  }
  .chip.on { background: var(--accent-gradient); color: white; border-color: transparent; }
</style>
