<script lang="ts">
  // Downloads: your servers' queues, episodes saved offline, and what's
  // downloading here; several picked to remove together.
  import Button from "../lib/Button.svelte";
  import Poster from "../lib/Poster.svelte";
  import { press, typed } from "../lib/api";

  let { page }: { page: any } = $props();

  let picked = $state(new Set<string>());
  let open = $state(new Set<string>());
  function toggle(set: Set<string>, key: string) {
    const s = new Set(set);
    if (s.has(key)) s.delete(key);
    else s.add(key);
    return s;
  }
  function pickAll(cards: any[], on: boolean) {
    const s = new Set(picked);
    for (const c of cards) if (c.key) on ? s.add(c.key) : s.delete(c.key);
    picked = s;
  }
  function removePicked(cards: any[]) {
    const keys = cards.filter((c) => c.key && picked.has(c.key)).map((c) => c.key);
    typed(page.remove_many, keys.join(","));
    picked = new Set();
  }
</script>

<div class="page">
  <p class="muted small">{page.summary}</p>
  {#if page.set_up}<p class="muted">{page.set_up}</p>{/if}
  {#each page.notes as n}<p class="small {n.tone}">{n.text}</p>{/each}

  {#each page.sections as s}
    {@const keyed = s.cards.filter((c: any) => c.key)}
    {@const chosen = keyed.filter((c: any) => picked.has(c.key))}
    {#if s.title}<h2>{s.title} <span class="muted small">{s.note}</span></h2>{/if}
    {#if s.empty}<p class="muted empty">{s.empty}</p>{/if}
    {#if keyed.length > 0}
      <div class="pickbar">
        <button class="check" onclick={() => pickAll(keyed, chosen.length < keyed.length)}>
          <span class="tick" class:on={chosen.length === keyed.length}>{chosen.length === keyed.length ? "✓" : ""}</span>
          Select all
        </button>
        {#if chosen.length}
          <span class="muted small">{chosen.length} selected</span>
          <span class="grow"></span>
          <button class="danger" onclick={() => removePicked(keyed)}>Remove selected…</button>
        {/if}
      </div>
    {/if}
    {#each s.cards as c, i (c.key ?? i)}
      <div class="card">
        <div class="row">
          {#if c.key}
            <button class="pick" onclick={() => (picked = toggle(picked, c.key))} aria-label="Pick">
              <span class="tick" class:on={picked.has(c.key)}>{picked.has(c.key) ? "✓" : ""}</span>
            </button>
          {/if}
          <span class="thumb"><Poster src={c.cover} radius={6} /></span>
          <span class="info">
            <span class="src {c.tone}">{c.source}</span>
            <b>{c.title}</b>
            {#if c.which}<span class="muted small">{c.which}</span>{/if}
          </span>
        </div>
        <span class="bar {c.tone}"><span style="width: {c.fraction * 100}%"></span></span>
        <span class="muted small">{c.detail}</span>
        {#if c.release}<span class="faint small break">{c.release}</span>{/if}
        {#if c.problem}<span class="small warn">{c.problem}</span>{/if}
        {#if c.note}<span class="small faint">{c.note}</span>{/if}
        <div class="buttons">
          {#each c.buttons as b}
            {#if b.danger}<Button {b} kind="danger" small />{:else}<Button {b} small />{/if}
          {/each}
          {#if c.remove}<Button b={c.remove} kind="danger" small />{/if}
          {#if c.files?.length}
            <button class="link" onclick={() => (open = toggle(open, c.key))}>{open.has(c.key) ? "Hide files" : `${c.files.length} files`}</button>
          {/if}
        </div>
        {#if c.files && open.has(c.key)}
          {#each c.files as f}<span class="file"><span class="faint small">{Math.round(f.fraction * 100)}%</span> {f.path}</span>{/each}
        {/if}
      </div>
    {/each}
  {/each}
</div>

<style>
  .page { padding: 14px 16px 28px; display: flex; flex-direction: column; gap: 10px; }
  p { margin: 0; }
  h2 { margin: 12px 0 0; display: flex; gap: 10px; align-items: baseline; flex-wrap: wrap; }
  .empty { text-align: center; padding: 40px 0; }
  .warn { color: var(--gold); }
  .error { color: var(--red); }
  .faint { color: var(--faint); }
  .break { word-break: break-word; }
  .pickbar { display: flex; align-items: center; gap: 10px; }
  .grow { flex: 1; }
  .check { display: flex; align-items: center; gap: 10px; background: none; border: none; color: var(--text); font: inherit; padding: 0; }
  .tick { width: 20px; height: 20px; border-radius: 5px; border: 1px solid var(--line); background: var(--surface2); display: grid; place-items: center; font-size: 13px; color: white; flex: none; }
  .tick.on { background: var(--accent); border-color: var(--accent); }
  .danger { font: inherit; font-size: 13px; font-weight: 600; color: var(--red); background: none; border: 1px solid var(--line); border-radius: 8px; padding: 6px 10px; }
  .card { background: var(--surface); border: 1px solid var(--line); border-radius: 12px; padding: 12px; display: flex; flex-direction: column; gap: 6px; }
  .row { display: flex; gap: 10px; align-items: flex-start; }
  .pick { background: none; border: none; padding: 2px 0 0; }
  .thumb { width: 40px; flex: none; }
  .info { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .info b { font-size: 14px; word-break: break-word; }
  .src { font-size: 10px; font-weight: 600; letter-spacing: 0.03em; color: var(--muted); }
  .src.good { color: var(--green); }
  .src.warn { color: var(--gold); }
  .src.error { color: var(--red); }
  .src.blue { color: var(--blue); }
  .src.accent { color: var(--accent); }
  .bar { display: block; height: 6px; border-radius: 3px; background: var(--surface2); overflow: hidden; }
  .bar span { display: block; height: 100%; background: var(--green); }
  .bar.warn span { background: var(--gold); }
  .bar.error span { background: var(--red); }
  .bar.blue span { background: var(--blue); }
  .bar.accent span { background: var(--accent); }
  .buttons { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; margin-top: 2px; }
  .link { background: none; border: none; color: var(--accent); font: inherit; font-size: 13px; padding: 2px 4px; }
  .file { font-size: 12px; color: var(--muted); word-break: break-word; }
</style>
