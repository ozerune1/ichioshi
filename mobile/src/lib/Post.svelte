<script lang="ts">
  // A post: who and when, its text, quotes, spoilers (shown when tapped)
  // and pictures; the replies to it, indented a little.
  import Self from "./Post.svelte";
  import { press } from "./api";

  let { p, depth = 0 }: { p: any; depth?: number } = $props();
  let shown = $state(false);
</script>

<div class="post">
  <div class="meta">
    <b>{p.author}</b>
    <span class="faint">{p.when}</span>
    {#if p.likes}<span class="muted">♥ {p.likes}</span>{/if}
    <span class="grow"></span>
    {#if p.reply}<button class="reply" onclick={() => press(p.reply)}>Reply</button>{/if}
  </div>
  {#each p.body as piece}
    {#if piece.t === "text"}
      <p>{piece.text}</p>
    {:else if piece.t === "quote"}
      <blockquote>{#if piece.who}<b>{piece.who} said:</b>{/if}<span>{piece.text}</span></blockquote>
    {:else if piece.t === "spoiler"}
      {#if shown}<p class="spoiler">{piece.text}</p>
      {:else}<button class="show" onclick={() => (shown = true)}>Show spoiler</button>{/if}
    {:else if piece.t === "image"}
      <img src={piece.url} alt="" loading="lazy" />
    {/if}
  {/each}
</div>
{#if p.replies.length}
  <div class="replies" class:indent={depth < 3}>
    {#each p.replies as r (r.id)}<Self p={r} depth={depth + 1} />{/each}
  </div>
{/if}

<style>
  .post { background: var(--surface); border: 1px solid var(--line); border-radius: 10px; padding: 12px; display: flex; flex-direction: column; gap: 8px; }
  .meta { display: flex; gap: 8px; align-items: center; font-size: 13px; }
  .grow { flex: 1; }
  .faint { color: var(--faint); font-size: 12px; }
  .muted { color: var(--muted); font-size: 12px; }
  p { margin: 0; font-size: 14px; line-height: 1.5; white-space: pre-wrap; }
  blockquote { margin: 0; padding: 8px 12px; border-radius: 8px; background: rgba(31, 36, 48, 0.6); border: 1px solid var(--line); display: flex; flex-direction: column; gap: 4px; font-size: 13px; color: var(--muted); }
  blockquote b { font-size: 12px; }
  .spoiler { color: var(--gold); background: rgba(251, 191, 36, 0.06); padding: 6px 10px; border-radius: 8px; }
  .show, .reply { align-self: flex-start; font: inherit; font-size: 12px; padding: 4px 10px; border-radius: 8px; border: 1px solid var(--line); background: var(--surface2); color: var(--text); }
  img { max-width: 100%; max-height: 380px; border-radius: 6px; object-fit: contain; align-self: flex-start; }
  .replies { display: flex; flex-direction: column; gap: 8px; margin-top: 8px; }
  .replies.indent { padding-left: 14px; border-left: 2px solid var(--line); margin-left: 4px; }
</style>
