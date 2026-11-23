<script lang="ts">
  // An episode's discussion, over the page: AniList's thread or
  // MyAnimeList's, and commenting on AniList.
  import Button from "./Button.svelte";
  import Chips from "./Chips.svelte";
  import Field from "./Field.svelte";
  import Post from "./Post.svelte";
  import { press } from "./api";

  let { d }: { d: any } = $props();
</script>

<div class="panel">
  <header>
    <button class="icon" onclick={() => press(d.close)} aria-label="Close">←</button>
    <div class="names"><b>{d.title}</b><span>{d.series}</span></div>
  </header>
  <div class="body">
    <Chips options={d.tabs} />
    {#if d.state}<p class="muted">{d.state}</p>{/if}
    {#if d.error}<p class="error">{d.error}</p>{/if}
    {#if d.look}<button class="link" onclick={() => press(d.look.id)}>{d.look.label}</button>{/if}
    {#if d.thread}
      {@const t = d.thread}
      <div class="thead">
        <b>{t.title}</b>
        <button class="link" onclick={() => press(t.reply_there.id)}>{t.reply_there.label}</button>
      </div>
      {#if t.composer}
        {#if t.composer.signin}
          <p class="muted small">{t.composer.signin}</p>
        {:else}
          <div class="composer">
            {#if t.composer.replying}
              <div class="replying"><span>{t.composer.replying}</span><button class="link" onclick={() => press(t.composer.cancel)}>Cancel</button></div>
            {/if}
            <form class="row" onsubmit={(e) => { e.preventDefault(); if (t.composer.post.id) press(t.composer.post.id); }}>
              <div class="grow"><Field f={t.composer.draft} hint={t.composer.hint} /></div>
              <Button b={t.composer.post} kind="primary" />
            </form>
          </div>
        {/if}
      {/if}
      {#each t.posts as p (p.id)}<Post {p} />{/each}
      {#if t.more}<Button b={t.more} />{/if}
    {/if}
  </div>
</div>

<style>
  .panel { position: fixed; inset: 0; z-index: 40; background: var(--bg); display: flex; flex-direction: column; }
  header { display: flex; align-items: center; gap: 4px; height: 56px; padding: 0 8px; background: var(--bars); flex: none; }
  .icon { width: 40px; height: 40px; border-radius: 20px; border: none; background: none; color: var(--text); font-size: 20px; }
  .names { display: flex; flex-direction: column; min-width: 0; }
  .names b { font-size: 17px; }
  .names span { font-size: 12px; color: var(--muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .body { flex: 1; overflow-y: auto; padding: 12px 16px 28px; display: flex; flex-direction: column; gap: 12px; }
  .muted { color: var(--muted); margin: 0; }
  .error { color: var(--red); margin: 0; }
  .small { font-size: 13px; }
  .link { align-self: flex-start; background: none; border: none; color: var(--accent); font: inherit; font-size: 13px; padding: 2px 0; text-align: left; }
  .thead { display: flex; flex-direction: column; gap: 4px; }
  .thead b { font-size: 15px; }
  .composer { display: flex; flex-direction: column; gap: 6px; }
  .replying { display: flex; gap: 10px; align-items: center; font-size: 12px; color: var(--accent); font-weight: 600; }
  .row { display: flex; gap: 8px; align-items: flex-end; }
  .grow { flex: 1; min-width: 0; }
</style>
