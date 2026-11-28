<script lang="ts">
  // Chips tapped round three ways (from the view: label, state, handle):
  // off, wanted ("in", +), not wanted ("out", −).
  import { press } from "./api";
  let { options }: { options: { label: string; state: "in" | "out" | null; id: string }[] } = $props();
</script>

<div class="chips">
  {#each options as o (o.label)}
    <button class="chip" class:in={o.state === "in"} class:out={o.state === "out"} onclick={() => press(o.id)}>
      {o.state === "in" ? "+ " : o.state === "out" ? "− " : ""}{o.label}
    </button>
  {/each}
</div>

<style>
  .chips { display: flex; flex-wrap: wrap; gap: 8px; }
  .chip {
    flex: none; font: inherit; font-size: 13px; font-weight: 500; padding: 7px 13px; border-radius: 999px;
    border: 1px solid var(--line); background: var(--surface); color: var(--muted); white-space: nowrap;
  }
  .chip.in { background: var(--accent-gradient); color: white; border-color: transparent; }
  .chip.out { background: color-mix(in srgb, var(--red) 12%, transparent); color: var(--red); border-color: color-mix(in srgb, var(--red) 60%, transparent); }
</style>
