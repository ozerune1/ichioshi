<script lang="ts">
  // A cover image (a placeholder while there's none). One that doesn't load
  // (the phone's connection dropped, say) is tried again a few times, a
  // little later each time.
  let { src, ratio = "176 / 250", radius = 10 }: { src: string | null | undefined; ratio?: string; radius?: number } = $props();

  let tries = $state(0);
  let failed = $state(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  // A new picture starts afresh.
  $effect(() => {
    void src;
    tries = 0;
    failed = false;
    return () => clearTimeout(timer);
  });
  function missed() {
    failed = true;
    if (tries >= 4) return;
    clearTimeout(timer);
    timer = setTimeout(() => {
      tries += 1;
      failed = false;
    }, 3000 * 2 ** tries);
  }
  // The same address again, but not from the browser's memory of failing.
  let url = $derived(src && tries ? `${src}${src.includes("?") ? "&" : "?"}try=${tries}` : src);
</script>

<div class="poster" style="aspect-ratio: {ratio}; border-radius: {radius}px">
  {#if url && !failed}<img src={url} alt="" loading="lazy" onerror={missed} />{/if}
</div>

<style>
  .poster { background: var(--surface2); overflow: hidden; width: 100%; }
  img { width: 100%; height: 100%; object-fit: cover; display: block; }
</style>
