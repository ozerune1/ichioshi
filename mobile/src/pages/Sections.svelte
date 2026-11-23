<script lang="ts">
  // A page made of titled sections of items (the playlist, remuxing).
  import Form from "../lib/Form.svelte";
  let { page }: { page: any } = $props();
</script>

<div class="page">
  {#each page.sections as s}
    {#if s.items.length}
      {#if s.title}<h2>{s.title}</h2>{/if}
      <Form items={s.items} />
    {/if}
  {/each}
</div>

<style>
  .page { padding: 14px 16px 28px; display: flex; flex-direction: column; gap: 12px; }
  h2 { margin: 12px 0 0; }
</style>
