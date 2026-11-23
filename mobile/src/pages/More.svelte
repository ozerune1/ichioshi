<script lang="ts">
  // More: how things are, the playlist, and the settings' groups (each a
  // page of its own).
  import Form from "../lib/Form.svelte";
  import { press } from "../lib/api";
  import { app } from "../lib/store.svelte";

  let { page }: { page: any } = $props();

  let open = $derived(page.sections.find((s: any) => s.key === app.section?.key));
</script>

<div class="page">
  {#if open}
    <Form items={open.items} />
  {:else}
    <div class="status">
      {#each page.status as line}
        <p class={line.tone}><span class="dot"></span>{line.text}</p>
      {/each}
    </div>

    <div class="list">
      <button class="row" onclick={() => press(page.playlist.id)}>
        <span class="icon">≡</span>
        <span class="what"><b>Playlist</b><span>{page.playlist.note}</span></span>
        <span class="go">›</span>
      </button>
    </div>

    <p class="label">Settings</p>
    <div class="list">
      {#each page.sections as s (s.key)}
        <button class="row" onclick={() => (app.section = { key: s.key, name: s.name })}>
          <span class="icon">{s.icon}</span>
          <span class="what"><b>{s.name}</b><span>{s.note}</span></span>
          <span class="go">›</span>
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .page { padding: 14px 16px 28px; }
  .status { background: var(--surface); border: 1px solid var(--line); border-radius: 12px; padding: 12px 14px; }
  .status p { margin: 0; font-size: 13px; display: flex; align-items: center; gap: 8px; color: var(--muted); }
  .status p.faint { color: var(--faint); font-size: 12px; padding-left: 16px; }
  .dot { width: 8px; height: 8px; border-radius: 4px; background: var(--muted); }
  .good .dot { background: var(--green); }
  .warn .dot { background: var(--gold); }
  .error .dot { background: var(--red); }
  .faint .dot { display: none; }
  .label { font-size: 12px; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; color: var(--faint); margin: 22px 0 8px; }
  .list { background: var(--surface); border: 1px solid var(--line); border-radius: 12px; margin-top: 14px; overflow: hidden; }
  .label + .list { margin-top: 0; }
  .row {
    width: 100%; display: flex; align-items: center; gap: 12px; padding: 13px 14px;
    background: none; border: none; color: var(--text); font: inherit; text-align: left;
  }
  .row + .row { border-top: 1px solid var(--line); }
  .row:active { background: rgba(255, 255, 255, 0.05); }
  .icon { width: 24px; text-align: center; color: var(--accent); font-size: 16px; }
  .what { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
  .what span { font-size: 12px; color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .go { color: var(--faint); font-size: 22px; }
</style>
