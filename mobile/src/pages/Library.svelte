<script lang="ts">
  // The library: how it is, what's next to watch, and your series.
  import Button from "../lib/Button.svelte";
  import Poster from "../lib/Poster.svelte";
  import { goTab, press } from "../lib/api";
  import { app } from "../lib/store.svelte";

  let { page }: { page: any } = $props();

  let search = $state("");
  // The list status filtered by, remembered (a big library is mostly what
  // you're not watching).
  const KEY = "library-filter";
  function remembered(): string | null {
    try {
      return localStorage.getItem(KEY);
    } catch {
      return null;
    }
  }
  let only = $state<string | null>(remembered());
  function filter(k: string | null) {
    only = k;
    try {
      if (k) localStorage.setItem(KEY, k);
      else localStorage.removeItem(KEY);
    } catch {}
  }
  // Lists' sign-ins are in More › Lists.
  function signIn() {
    app.section = { key: "lists", name: "Lists" };
    goTab("more");
  }
  const statuses = ["watching", "completed", "planning", "paused", "dropped"];
  // A status (with lists), or "hentai" (18+ series kept off them).
  let filtering = $derived(
    (page.lists && !!only && statuses.includes(only)) || (page.adult_tab && only === "hentai"),
  );
  let tabs = $derived(page.lists || page.adult_tab);
  // The type shown ("Movie"), or all; the types the library has, by count.
  let type = $state<string | null>(null);
  let types = $derived(
    Object.entries(
      page.series.reduce((n: Record<string, number>, s: any) => {
        if (s.type) n[s.type] = (n[s.type] ?? 0) + 1;
        return n;
      }, {}),
    ) as [string, number][],
  );
  let shown = $derived(
    page.series.filter(
      (s: any) =>
        (!search || s.title.toLowerCase().includes(search.toLowerCase())) &&
        (!filtering || (only === "hentai" ? s.adult : s.status?.key === only)) &&
        (!type || s.type === type),
    ),
  );
  let counts = $derived(
    Object.fromEntries(
      statuses.map((k) => [k, page.series.filter((s: any) => s.status?.key === k).length]),
    ),
  );
</script>

<div class="page">
  <p class="muted small">{page.summary}</p>
  {#if page.about.servers}
    <p class="small about">
      On <b>{page.about.servers.join(" · ") || "no servers yet"}</b>
      <span class="faint">{page.about.read}</span>
      <Button b={page.about.refresh} kind="link" small />
    </p>
    {#each page.about.problems as p}<p class="small error">{p}</p>{/each}
  {:else}
    <p class="small about">
      <span class="muted">Languages</span> <b>{page.about.languages}</b>
      <Button b={page.about.change} kind="link" small />
    </p>
  {/if}

  {#if page.empty}
    <div class="empty">
      <h2>Your library is empty</h2>
      <p class="muted">Find an anime in Discover, then add it or download episodes of it.</p>
    </div>
  {:else}
    <input class="search" placeholder="Search your library" bind:value={search} />

    {#if page.continue.length && !search && (!filtering || only === "watching")}
      <h2>Continue watching</h2>
      <div class="shelf">
        {#each page.continue as c (c.id)}
          <div class="cont">
            <button class="art" onclick={() => press(c.open)}>
              <Poster src={c.art} ratio="16 / 9" radius={12} />
              <span class="shade"></span>
              <span class="cap">
                <b>{c.title}</b>
                <span>{c.line}</span>
                {#if c.progress > 0}<span class="bar"><span style="width: {c.progress * 100}%"></span></span>{/if}
              </span>
            </button>
            <button class="play" onclick={() => press(c.play)} aria-label="Play">▶</button>
          </div>
        {/each}
      </div>
    {/if}

    {#if tabs || types.length > 1}
      <div class="chips">
        {#if tabs}
          <button class="chip" class:on={!filtering} onclick={() => filter(null)}>All {page.series.length}</button>
        {/if}
        {#if page.lists}
          {#each statuses as k}
            {#if counts[k] || k !== "dropped"}
              <button class="chip" class:on={only === k} onclick={() => filter(k)}>
                {k[0].toUpperCase() + k.slice(1)} {counts[k]}
              </button>
            {/if}
          {/each}
        {/if}
        {#if page.adult_tab}
          <button class="chip" class:on={only === "hentai"} onclick={() => filter("hentai")}>
            Hentai {page.series.filter((s: any) => s.adult).length}
          </button>
        {/if}
        {#if types.length > 1}
          <select class="types" onchange={(e) => (type = (e.target as HTMLSelectElement).value || null)}>
            <option value="" selected={!type}>All types</option>
            {#each types as [t, n]}<option value={t} selected={type === t}>{t} {n}</option>{/each}
          </select>
        {/if}
      </div>
    {/if}
    {#if page.lists}
      {#if filtering && !shown.length}
        <p class="muted small none">
          {page.listsLoading ? "Reading your lists…" : search ? "Nothing matches." : "Nothing with this status."}
        </p>
      {/if}
    {:else}
      <p class="small hint">
        <span class="muted">To filter by Watching, Completed and the rest, and keep dropped shows off the calendar:</span>
        <button class="link" onclick={signIn}>sign in to AniList or MyAnimeList</button>
      </p>
    {/if}

    <div class="grid">
      {#each shown as s (s.id)}
        <button class="card" onclick={() => press(s.open)}>
          <div class="cover">
            <Poster src={s.cover} />
            {#if s.status}<span class="status" style="background: var(--{s.status.key})">{s.status.name}</span>{/if}
            {#if s.downloading !== null}<span class="dl">↓ {Math.round(s.downloading * 100)}%</span>{/if}
            {#if s.watched > 0}<span class="watched"><span style="width: {s.watched * 100}%"></span></span>{/if}
          </div>
          <span class="t">{s.title}</span>
          <span class="m">{s.meta}</span>
          {#if s.airing}<span class="a">{s.airing}</span>{/if}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .types { flex: none; font: inherit; font-size: 13px; padding: 7px 10px; border-radius: 999px; border: 1px solid var(--line); background: var(--surface); color: var(--text); }
  .page { padding: 14px 16px 24px; }
  p { margin: 0 0 6px; }
  .about { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; }
  .error { color: var(--red); }
  .empty { text-align: center; margin-top: 18vh; }
  .search {
    width: 100%; margin: 10px 0 4px; padding: 10px 14px; border-radius: 10px;
    border: 1px solid var(--line); background: var(--surface); color: var(--text);
  }
  .shelf { display: flex; gap: 12px; overflow-x: auto; scroll-snap-type: x mandatory; padding-bottom: 4px; }
  .cont { position: relative; flex: none; width: min(86vw, 360px); scroll-snap-align: start; }
  .art { display: block; width: 100%; padding: 0; border: none; background: none; color: inherit; text-align: left; position: relative; }
  .shade { position: absolute; inset: 0; border-radius: 12px; background: linear-gradient(transparent 20%, rgba(13, 15, 20, 0.92)); }
  .cap { position: absolute; left: 14px; right: 60px; bottom: 12px; display: flex; flex-direction: column; gap: 2px; font-size: 12px; color: #d0d5e0; }
  .cap b { font-size: 16px; color: white; }
  .bar { height: 4px; border-radius: 2px; background: rgba(255, 255, 255, 0.2); margin-top: 6px; overflow: hidden; }
  .bar span { display: block; height: 100%; background: var(--accent); }
  .play {
    position: absolute; right: 12px; bottom: 12px; width: 44px; height: 44px; border-radius: 22px;
    border: none; background: var(--accent-gradient); color: white; font-size: 16px;
  }
  .chips { display: flex; gap: 8px; overflow-x: auto; margin: 14px 0 4px; padding-bottom: 2px; }
  .chip {
    flex: none; font: inherit; font-size: 13px; padding: 6px 12px; border-radius: 999px;
    border: 1px solid var(--line); background: var(--surface); color: var(--muted);
  }
  .chip.on { background: var(--accent-gradient); color: white; border-color: transparent; }
  .none { margin-top: 18px; text-align: center; }
  .hint { margin: 14px 0 0; }
  .link { background: none; border: none; padding: 0; font: inherit; color: var(--accent); }
  .grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 16px 10px; margin-top: 14px; }
  .card {
    display: flex; flex-direction: column; gap: 2px; min-width: 0;
    padding: 0; border: none; background: none; color: inherit; text-align: left; font: inherit;
  }
  .cover { position: relative; margin-bottom: 6px; }
  .status, .dl {
    position: absolute; left: 6px; font-size: 10px; font-weight: 600; color: white;
    padding: 2px 7px; border-radius: 999px;
  }
  .status { top: 6px; }
  .dl { bottom: 12px; background: rgba(13, 15, 20, 0.85); color: var(--green); }
  .watched { position: absolute; left: 8px; right: 8px; bottom: 6px; height: 3px; border-radius: 2px; background: rgba(255, 255, 255, 0.15); overflow: hidden; }
  .watched span { display: block; height: 100%; background: var(--green); }
  .t { font-size: 13px; font-weight: 600; line-height: 1.25; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
  .m { font-size: 11px; color: var(--muted); }
  .a { font-size: 11px; color: var(--faint); }
</style>
