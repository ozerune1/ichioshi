<script lang="ts">
  // A button from the view: pressing it sends its handle; greyed out, it
  // says why (if the view says).
  import { press, type Btn } from "./api";
  import { say } from "./store.svelte";

  let {
    b,
    kind = "secondary",
    small = false,
    wide = false,
  }: { b: Btn; kind?: "primary" | "secondary" | "danger" | "link"; small?: boolean; wide?: boolean } =
    $props();

  function go() {
    if (b.id) press(b.id);
    else if (b.why) say(b.why);
  }
</script>

<button class="btn {kind}" class:small class:wide class:off={!b.id} onclick={go}>
  {b.label}
</button>

<style>
  .btn {
    font: inherit;
    font-weight: 600;
    font-size: 14px;
    border-radius: 10px;
    padding: 10px 16px;
    border: 1px solid var(--line);
    background: var(--surface2);
    color: var(--text);
    white-space: nowrap;
  }
  .btn.small { font-size: 13px; padding: 6px 12px; border-radius: 8px; }
  .btn.wide { width: 100%; }
  .primary { border: none; background: var(--accent-gradient); color: white; }
  .danger { color: var(--red); background: transparent; }
  .link { border: none; background: none; color: var(--accent); padding: 4px 2px; font-weight: 500; }
  .off { opacity: 0.45; }
  .btn:active:not(.off) { filter: brightness(1.2); }
</style>
