<script lang="ts">
  // Choosing a release's files (or episodes) to download.
  import Button from "../lib/Button.svelte";
  import Field from "../lib/Field.svelte";
  import { press } from "../lib/api";

  let { page }: { page: any } = $props();
</script>

<div class="wrap">
  <div class="page">
    <b class="title">{page.title}</b>
    {#if page.subtitle}<p class="muted small">{page.subtitle}</p>{/if}
    {#each page.notes as n}<p class="small {n.tone}">{n.text}</p>{/each}
    <div class="tools">
      {#each page.tools as t}<button class="tool" onclick={() => press(t.id)}>{t.label}</button>{/each}
    </div>
    <Field f={page.filter} hint="Filter, e.g. S01E0 or NCOP" />
    <p class="faint small">{page.heading}</p>
    {#each page.rows as r}
      <button class="row" class:on={r.on} onclick={() => press(r.id)}>
        <span class="box" class:on={r.on}>{r.on ? "✓" : ""}</span>
        <span class="info"><span class="label">{r.label}</span><span class="meta">{r.meta}</span></span>
      </button>
    {/each}
  </div>
  <div class="foot">
    <b>{page.summary}</b>
    <span class="muted small">{page.folder}</span>
    {#if page.why}<span class="small warn">{page.why}</span>{/if}
    <Button b={page.go} kind="primary" wide />
  </div>
</div>

<style>
  .wrap { min-height: 100%; display: flex; flex-direction: column; }
  .page { padding: 14px 16px; display: flex; flex-direction: column; gap: 10px; flex: 1; }
  .title { font-size: 16px; }
  p { margin: 0; }
  .good { color: var(--green); }
  .warn { color: var(--gold); }
  .muted { color: var(--muted); }
  .tools { display: flex; flex-wrap: wrap; gap: 8px; }
  .tool { font: inherit; font-size: 13px; padding: 6px 12px; border-radius: 8px; border: 1px solid var(--line); background: var(--surface2); color: var(--text); }
  .row { display: flex; gap: 12px; align-items: flex-start; padding: 10px; border-radius: 10px; border: none; background: var(--surface); color: inherit; font: inherit; text-align: left; }
  .row.on { background: rgba(139, 124, 255, 0.1); }
  .box { flex: none; width: 22px; height: 22px; border-radius: 6px; border: 1px solid var(--line); background: var(--surface2); display: grid; place-items: center; font-size: 13px; margin-top: 1px; }
  .box.on { background: var(--accent); border-color: var(--accent); color: white; }
  .info { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
  .label { font-size: 14px; word-break: break-word; }
  .meta { font-size: 12px; color: var(--muted); }
  .foot { position: sticky; bottom: 0; padding: 12px 16px; background: var(--bars); border-top: 1px solid var(--line); display: flex; flex-direction: column; gap: 6px; }
</style>
