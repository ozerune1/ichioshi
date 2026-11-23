<script lang="ts">
  // A choice from a list (from the view: label, on, handle).
  import { press } from "./api";
  let { options, label = "" }: { options: { label: string; on: boolean; id: string | null }[]; label?: string } = $props();
</script>

<label class="select">
  {#if label}<span class="name">{label}</span>{/if}
  <select
    onchange={(e) => {
      const id = options[(e.target as HTMLSelectElement).selectedIndex]?.id;
      if (id) press(id);
    }}
  >
    {#each options as o}<option selected={o.on} disabled={!o.id}>{o.label}</option>{/each}
  </select>
</label>

<style>
  .select { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
  .name { font-size: 13px; color: var(--muted); }
  select {
    padding: 10px 12px; border-radius: 10px; border: 1px solid var(--line);
    background: var(--surface); color: var(--text); font-size: 14px; min-width: 0;
  }
</style>
