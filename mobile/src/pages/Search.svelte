<script lang="ts">
  // Searching AniList.
  import Button from "../lib/Button.svelte";
  import Field from "../lib/Field.svelte";
  import Poster from "../lib/Poster.svelte";
  import { press } from "../lib/api";

  let { page }: { page: any } = $props();
</script>

<div class="page">
  <form class="bar" onsubmit={(e) => { e.preventDefault(); if (page.go.id) press(page.go.id); }}>
    <div class="grow"><Field f={page.query} hint="Search anime, e.g. Frieren" /></div>
    <Button b={page.go} kind="primary" />
  </form>
  <p class="muted small">{page.note}</p>
  {#if page.nothing}<p class="muted">{page.nothing}</p>{/if}
  {#each page.results as r}
    <button class="result" onclick={() => press(r.open)}>
      <span class="thumb"><Poster src={r.cover} radius={8} /></span>
      <span class="info">
        <b>{r.title}</b>
        {#if r.alt}<span class="muted small">{r.alt}</span>{/if}
        <span class="muted small">{r.summary}</span>
        <span class="best" class:good={r.good}>{r.best}</span>
        {#if r.in_library}<span class="lib">In your library</span>{/if}
      </span>
    </button>
  {/each}
</div>

<style>
  .page { padding: 14px 16px 28px; display: flex; flex-direction: column; gap: 12px; }
  .bar { display: flex; gap: 10px; align-items: flex-end; }
  .grow { flex: 1; min-width: 0; }
  p { margin: 0; }
  .result { display: flex; gap: 12px; padding: 10px; border-radius: 12px; border: 1px solid var(--line); background: var(--surface); color: inherit; font: inherit; text-align: left; }
  .thumb { width: 60px; flex: none; }
  .info { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
  .info b { font-size: 15px; }
  .best { font-size: 12px; color: var(--muted); }
  .best.good { color: var(--gold); }
  .lib { font-size: 11px; font-weight: 600; color: var(--green); }
</style>
