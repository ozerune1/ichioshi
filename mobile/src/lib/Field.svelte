<script lang="ts">
  // A text field from the view: what's typed goes to the core as it's
  // typed; while typing, the view's own text (a moment behind) is ignored.
  import { typed, type Field } from "./api";

  let { f, label = "", hint = "", secret = false }: { f: Field; label?: string; hint?: string; secret?: boolean } =
    $props();

  let value = $state("");
  let editing = $state(false);
  let shown = $state(false);
  $effect(() => {
    if (!editing) value = f.value;
  });

  function onInput(e: Event) {
    value = (e.target as HTMLInputElement).value;
    typed(f.id, value);
  }
</script>

<label class="field">
  {#if label}<span class="name">{label}</span>{/if}
  <span class="row">
    <input
      type={secret && !shown ? "password" : "text"}
      {value}
      placeholder={hint}
      oninput={onInput}
      onfocus={() => (editing = true)}
      onblur={() => (editing = false)}
      autocapitalize="off"
      autocomplete="off"
      spellcheck="false"
    />
    {#if secret}
      <button type="button" class="eye" onclick={() => (shown = !shown)}>{shown ? "Hide" : "Show"}</button>
    {/if}
  </span>
</label>

<style>
  .field { display: flex; flex-direction: column; gap: 6px; }
  .name { font-size: 13px; color: var(--muted); }
  .row { display: flex; gap: 8px; }
  input {
    flex: 1; min-width: 0; padding: 12px 14px; border-radius: 10px;
    border: 1px solid var(--line); background: var(--bg); color: var(--text); font-size: 15px;
  }
  input:focus { outline: none; border-color: var(--accent); }
  .eye {
    flex: none; font: inherit; font-size: 13px; padding: 0 12px; border-radius: 10px;
    border: 1px solid var(--line); background: var(--surface2); color: var(--muted);
  }
</style>
