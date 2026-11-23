<script lang="ts">
  // A dialog from the view, as a sheet from the bottom: tapping above it
  // (or back) closes it.
  import Button from "./Button.svelte";
  import Form from "./Form.svelte";
  import { press } from "./api";

  let { d }: { d: any } = $props();
</script>

<div class="dim" role="presentation" onclick={() => press(d.close)}></div>
<div class="sheet">
  <span class="handle"></span>
  <div class="body">
    <h3>{d.title}</h3>
    {#if d.note}<p class="note">{d.note}</p>{/if}
    {#if d.items?.length}<Form items={d.items} />{/if}
  </div>
  {#if d.buttons?.length}
    <div class="buttons">
      {#each d.buttons as b}<Button {b} kind={b.kind ?? "secondary"} />{/each}
    </div>
  {/if}
</div>

<style>
  .dim { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.55); z-index: 30; }
  .sheet {
    position: fixed; left: 0; right: 0; bottom: 0; z-index: 31; max-height: calc(100% - 56px);
    display: flex; flex-direction: column; background: var(--surface);
    border: 1px solid var(--line); border-bottom: none; border-radius: 20px 20px 0 0;
    animation: up 0.18s ease-out;
  }
  @keyframes up { from { transform: translateY(40px); opacity: 0; } }
  .handle { align-self: center; width: 36px; height: 4px; border-radius: 2px; background: rgba(255, 255, 255, 0.25); margin: 10px 0 4px; flex: none; }
  .body { overflow-y: auto; padding: 6px 18px 12px; display: flex; flex-direction: column; gap: 12px; }
  h3 { margin: 0; font-size: 20px; }
  .note { margin: 0; color: var(--muted); font-size: 13px; }
  .buttons { display: flex; flex-wrap: wrap; gap: 10px; justify-content: flex-end; padding: 12px 18px 16px; border-top: 1px solid var(--line); flex: none; }
</style>
