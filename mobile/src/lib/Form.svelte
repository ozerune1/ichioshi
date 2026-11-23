<script lang="ts">
  // Settings items from the view: text, choices, fields, switches,
  // buttons, links, text to copy, and boxes of them.
  import Button from "./Button.svelte";
  import Field from "./Field.svelte";
  import Self from "./Form.svelte";
  import { press } from "./api";
  import { say } from "./store.svelte";

  let { items }: { items: any[] } = $props();

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      const t = document.createElement("textarea");
      t.value = text;
      document.body.appendChild(t);
      t.select();
      document.execCommand("copy");
      t.remove();
    }
    say("Copied");
  }
</script>

<div class="form">
  {#each items as it}
    {#if it.t === "text"}
      <p class="text {it.tone ?? ''}">{it.text}</p>
    {:else if it.t === "heading"}
      <div class="heading">
        <b>{it.text}</b>
        {#if it.sub}<span class="muted small">{it.sub}</span>{/if}
      </div>
    {:else if it.t === "chips"}
      <div class="chips">
        {#each it.options as o}
          <button class="chip" class:on={o.on} onclick={() => press(o.id)}>{o.label}</button>
        {/each}
      </div>
    {:else if it.t === "field"}
      <Field f={it} label={it.label} hint={it.hint} secret={it.secret} />
    {:else if it.t === "check"}
      <button class="check" onclick={() => press(it.id)}>
        <span class="switch" class:on={it.on}><span></span></span>
        <span>{it.label}</span>
      </button>
    {:else if it.t === "select"}
      <label class="select">
        <span class="name">{it.label}</span>
        <select onchange={(e) => press(it.options[(e.target as HTMLSelectElement).selectedIndex].id)}>
          {#each it.options as o}
            <option selected={o.on}>{o.label}</option>
          {/each}
        </select>
      </label>
    {:else if it.t === "buttons"}
      <div class="buttons">
        {#each it.buttons as b, i}
          <Button {b} small={it.small} kind={b.kind ?? (it.primary && i === 0 ? "primary" : "secondary")} />
        {/each}
      </div>
    {:else if it.t === "link"}
      <button class="link" onclick={() => press(it.id)}>{it.label}</button>
    {:else if it.t === "copy"}
      <div class="copy">
        <code>{it.value}</code>
        <button class="link" onclick={() => copy(it.value)}>Copy</button>
      </div>
    {:else if it.t === "box"}
      <div class="box"><Self items={it.items} /></div>
    {/if}
  {/each}
</div>

<style>
  .form { display: flex; flex-direction: column; gap: 14px; }
  .text { margin: 0; font-size: 14px; }
  .text.muted { color: var(--muted); font-size: 13px; }
  .text.faint { color: var(--faint); font-size: 12px; }
  .text.good { color: var(--green); }
  .text.warn { color: var(--gold); }
  .text.error { color: var(--red); }
  .heading { display: flex; flex-direction: column; gap: 2px; }
  .heading b { font-size: 16px; }
  .chips { display: flex; flex-wrap: wrap; gap: 8px; }
  .chip {
    font: inherit; font-size: 13px; font-weight: 500; padding: 8px 14px; border-radius: 999px;
    border: 1px solid var(--line); background: var(--surface); color: var(--muted);
  }
  .chip.on { background: var(--accent-gradient); color: white; border-color: transparent; }
  .check {
    display: flex; align-items: center; gap: 12px; text-align: left;
    background: none; border: none; color: var(--text); font: inherit; padding: 2px 0;
  }
  .switch { flex: none; width: 40px; height: 24px; border-radius: 12px; background: var(--surface2); border: 1px solid var(--line); position: relative; }
  .switch span { position: absolute; top: 3px; left: 3px; width: 16px; height: 16px; border-radius: 8px; background: var(--muted); transition: left 0.15s; }
  .switch.on { background: var(--accent); border-color: var(--accent); }
  .switch.on span { left: 19px; background: white; }
  .select { display: flex; flex-direction: column; gap: 6px; }
  .name { font-size: 13px; color: var(--muted); }
  select {
    padding: 12px 14px; border-radius: 10px; border: 1px solid var(--line);
    background: var(--bg); color: var(--text); font-size: 15px;
  }
  .buttons { display: flex; flex-wrap: wrap; gap: 10px; }
  .link { align-self: flex-start; background: none; border: none; color: var(--accent); font: inherit; font-size: 14px; padding: 2px 0; text-align: left; }
  .copy { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  code { font-family: ui-monospace, monospace; font-size: 13px; background: var(--surface2); padding: 4px 8px; border-radius: 6px; }
  .box { background: var(--surface); border: 1px solid var(--line); border-radius: 12px; padding: 14px; }
</style>
