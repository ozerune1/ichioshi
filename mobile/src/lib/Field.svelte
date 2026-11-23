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

  // What's typed goes to the core once typing pauses (and at once when
  // the field's left or submitted), not at every key.
  let pending: ReturnType<typeof setTimeout> | undefined;
  function send() {
    clearTimeout(pending);
    pending = undefined;
    if (value !== f.value) typed(f.id, value);
  }
  function onInput(e: Event) {
    value = (e.target as HTMLInputElement).value;
    clearTimeout(pending);
    pending = setTimeout(send, 250);
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
      onblur={() => {
        send();
        editing = false;
      }}
      onkeydown={(e) => e.key === "Enter" && send()}
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
