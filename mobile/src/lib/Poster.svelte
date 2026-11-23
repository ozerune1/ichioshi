<script lang="ts">
  // A cover image (a placeholder while there's none).
  let { src, ratio = "176 / 250", radius = 10 }: { src: string | null | undefined; ratio?: string; radius?: number } = $props();
</script>

<div class="poster" style="aspect-ratio: {ratio}; border-radius: {radius}px">
  {#if src}<img {src} alt="" loading="lazy" />{/if}
</div>

<style>
  .poster { background: var(--surface2); overflow: hidden; width: 100%; }
  img { width: 100%; height: 100%; object-fit: cover; display: block; }
</style>
