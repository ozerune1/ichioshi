<script lang="ts">
  // Discover: AniList's shelves, or the calendar (a week, a day at a time).
  import Chips from "../lib/Chips.svelte";
  import Poster from "../lib/Poster.svelte";
  import Select from "../lib/Select.svelte";
  import { press } from "../lib/api";

  let { page }: { page: any } = $props();

  // The calendar's day (today, or the week's first).
  let picked = $state<string | null>(null);
  let day = $derived(
    page.calendar
      ? (page.days.find((d: any) => d.date === picked) ??
          page.days.find((d: any) => d.today) ??
          page.days[0])
      : null,
  );
</script>

<div class="page">
  <div class="switch">
    <button class:on={!page.calendar} onclick={() => page.calendar && press(page.to_browse)}>Browse</button>
    <button class:on={page.calendar} onclick={() => !page.calendar && press(page.to_calendar)}>Calendar</button>
  </div>

  {#if !page.calendar}
    <Chips options={page.shelves} scroll />
    <div class="filters">
      <Select options={page.genres} />
      <Select options={page.formats} />
    </div>
    {#if page.state}
      <p class="state" class:error={page.state.error}>{page.state.text}</p>
      {#if page.state.retry}<button class="link" onclick={() => press(page.state.retry)}>Try again</button>{/if}
    {/if}
    <div class="grid">
      {#each page.list as m (m.id)}
        <button class="card" onclick={() => press(m.open)}>
          <div class="cover">
            <Poster src={m.cover} />
            <span class="badges">
              {#if m.status}<span class="pill" style="background: var(--{m.status.key})">{m.status.name}</span>{/if}
              {#if m.in_library}<span class="pill" style="background: var(--green)">In library</span>{/if}
            </span>
            {#if m.score}<span class="score">★ {m.score}%</span>{/if}
          </div>
          <span class="t">{m.title}</span>
          <span class="m">{m.facts}</span>
          {#if m.airing}<span class="air">{m.airing}</span>{:else if m.genres}<span class="m faint">{m.genres}</span>{/if}
        </button>
      {/each}
    </div>
  {:else}
    <div class="week">
      <button class="arrow" onclick={() => { picked = null; press(page.previous); }}>‹</button>
      <b>{page.range}</b>
      <button class="arrow" onclick={() => { picked = null; press(page.next); }}>›</button>
    </div>
    <div class="days">
      {#each page.days as d (d.date)}
        <button class="day" class:on={d === day} class:today={d.today} onclick={() => (picked = d.date)}>
          <span class="wd">{d.weekday}</span>
          <span class="n">{d.day}</span>
          <span class="dot" class:busy={d.items.length > 0}></span>
        </button>
      {/each}
    </div>
    <div class="options">
      <button class="check" onclick={() => press(page.full.id)}>
        <span class="box" class:on={page.full.on}>{page.full.on ? "✓" : ""}</span>Everything airing
      </button>
      {#if page.this_week}<button class="link" onclick={() => { picked = null; press(page.this_week); }}>This week</button>{/if}
    </div>
    <p class="faint small">Showing {page.whose}</p>
    {#if page.state}
      <p class="state" class:error={page.state.error}>{page.state.text}</p>
    {:else if day}
      <h2>{day.long}</h2>
      {#if day.items.length === 0}
        <p class="muted">{page.full.on ? "Nothing airs that day." : "Nothing of yours airs that day."}</p>
      {/if}
      {#each day.items as a}
        <button class="airing" class:aired={a.aired} onclick={() => press(a.open)}>
          <span class="thumb"><Poster src={a.cover} radius={6} /></span>
          <span class="info">
            <span class="time">{a.time}</span>
            <b>{a.title}</b>
            <span class="ep">{a.episode}{#if a.status} · <span style="color: var(--{a.status.key})">{a.status.name}</span>{/if}</span>
          </span>
        </button>
      {/each}
    {/if}
  {/if}
</div>

<style>
  .page { padding: 12px 16px 28px; display: flex; flex-direction: column; gap: 12px; }
  .switch { display: flex; gap: 4px; padding: 4px; border-radius: 12px; background: var(--surface); }
  .switch button { flex: 1; padding: 8px; border: none; border-radius: 9px; background: none; color: var(--muted); font: inherit; font-weight: 600; }
  .switch button.on { background: rgba(139, 124, 255, 0.22); color: var(--text); }
  .filters { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
  .state { color: var(--muted); }
  .state.error { color: var(--red); }
  .link { align-self: flex-start; background: none; border: none; color: var(--accent); font: inherit; padding: 0; }
  .grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 16px 10px; }
  .card { display: flex; flex-direction: column; gap: 2px; min-width: 0; padding: 0; border: none; background: none; color: inherit; text-align: left; font: inherit; }
  .cover { position: relative; margin-bottom: 6px; }
  .badges { position: absolute; top: 6px; left: 6px; right: 6px; display: flex; flex-direction: column; gap: 4px; align-items: flex-start; }
  .pill { font-size: 10px; font-weight: 600; color: white; padding: 2px 7px; border-radius: 999px; }
  .score { position: absolute; left: 6px; bottom: 6px; font-size: 10px; font-weight: 600; color: var(--gold); background: rgba(13, 15, 20, 0.85); padding: 2px 6px; border-radius: 999px; }
  .t { font-size: 13px; font-weight: 600; line-height: 1.25; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
  .m { font-size: 11px; color: var(--muted); }
  .faint { color: var(--faint); }
  .air { font-size: 11px; color: var(--accent); }
  .week { display: flex; align-items: center; gap: 8px; }
  .week b { flex: 1; text-align: center; font-size: 15px; }
  .arrow { width: 36px; height: 36px; border-radius: 10px; border: 1px solid var(--line); background: var(--surface); color: var(--text); font-size: 20px; }
  .days { display: grid; grid-template-columns: repeat(7, 1fr); gap: 4px; }
  .day { display: flex; flex-direction: column; align-items: center; gap: 3px; padding: 8px 0; border-radius: 10px; border: 1px solid transparent; background: none; color: var(--text); font: inherit; }
  .day.on { background: rgba(139, 124, 255, 0.18); border-color: rgba(139, 124, 255, 0.5); }
  .day .wd { font-size: 11px; color: var(--muted); }
  .day .n { font-size: 16px; font-weight: 600; }
  .day.today .n { color: var(--accent); }
  .day .dot { width: 5px; height: 5px; border-radius: 3px; }
  .day .dot.busy { background: var(--accent); }
  .options { display: flex; align-items: center; justify-content: space-between; }
  .check { display: flex; align-items: center; gap: 10px; background: none; border: none; color: var(--text); font: inherit; padding: 0; }
  .box { width: 20px; height: 20px; border-radius: 5px; border: 1px solid var(--line); background: var(--surface2); display: grid; place-items: center; font-size: 13px; }
  .box.on { background: var(--accent); border-color: var(--accent); color: white; }
  h2 { margin: 8px 0 0; font-size: 16px; }
  .airing { display: flex; gap: 10px; align-items: center; padding: 8px; border-radius: 10px; border: none; background: var(--surface); color: inherit; font: inherit; text-align: left; }
  .airing.aired { background: none; opacity: 0.7; }
  .thumb { width: 44px; flex: none; }
  .info { display: flex; flex-direction: column; gap: 1px; min-width: 0; }
  .info b { font-size: 14px; font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .time { font-size: 11px; font-weight: 600; color: var(--accent); }
  .aired .time { color: var(--faint); }
  .ep { font-size: 12px; color: var(--faint); }
</style>
