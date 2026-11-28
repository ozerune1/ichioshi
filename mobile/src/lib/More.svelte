<script lang="ts">
  // The end of a list read a page at a time: the next page is asked for when
  // this comes near the screen, and again while it stays there (pages too
  // short to scroll). After a failure, a button asks again.
  import { press } from "./api";

  let { more }: { more: any } = $props();
  let end: HTMLElement | undefined = $state();
  let near = $state(false);

  $effect(() => {
    if (!end) return;
    const watch = new IntersectionObserver(
      (entries) => (near = entries.some((e) => e.isIntersecting)),
      { rootMargin: "0px 0px 800px 0px" },
    );
    watch.observe(end);
    return () => watch.disconnect();
  });

  $effect(() => {
    if (near && more && !more.loading && !more.error) press(more.id);
  });
</script>

{#if more}
  <div class="end" bind:this={end}>
    {#if more.error}
      <p class="error">{more.error}</p>
      <button class="link" onclick={() => press(more.id)}>Try again</button>
    {:else}
      <p class="muted">Loading more…</p>
    {/if}
  </div>
{/if}

<style>
  .end { display: flex; flex-direction: column; gap: 6px; align-items: center; padding: 14px 0; }
  p { margin: 0; font-size: 13px; }
  .muted { color: var(--muted); }
  .error { color: var(--red); text-align: center; }
  .link { background: none; border: none; color: var(--accent); font: inherit; padding: 0; }
</style>
