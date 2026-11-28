<script lang="ts">
  // A series: what it is, what to do with it, your lists, its episodes,
  // and the release downloads come from.
  import Button from "../lib/Button.svelte";
  import Field from "../lib/Field.svelte";
  import Poster from "../lib/Poster.svelte";
  import Select from "../lib/Select.svelte";
  import Chips from "../lib/Chips.svelte";
  import Form from "../lib/Form.svelte";
  import { press } from "../lib/api";

  let { page }: { page: any } = $props();

  let more = $state(false);
  let releaseOpen = $state(false);
  // A new series: its description short, its release choice shut.
  let lastId = 0;
  $effect(() => {
    if (page.id !== lastId) {
      lastId = page.id;
      more = false;
      releaseOpen = false;
    }
  });
  let release = $derived(page.release);
  let showReleases = $derived(!release.chosen || releaseOpen);
</script>

<div class="hero">
  {#if page.banner}<img class="banner" src={page.banner} alt="" />{/if}
  <div class="shade"></div>
  <div class="head">
    <span class="cover"><Poster src={page.cover} radius={10} /></span>
    <span class="names">
      <b>{page.title}</b>
      {#if page.alt}<span>{page.alt}</span>{/if}
    </span>
  </div>
</div>

<div class="page">
  <p class="facts">{page.facts}</p>
  <div class="tags">
    {#each page.genres as g}<span class="genre">{g}</span>{/each}
    {#each page.links as l}<button class="link" onclick={() => press(l.id)}>{l.label}</button>{/each}
  </div>

  <div class="actions">
    {#each page.actions as b, i}<Button {b} kind={i === 0 ? "primary" : "secondary"} />{/each}
  </div>

  {#if page.lists}
    <div class="box lists">
      <div class="row2">
        <Select options={page.lists.status.length ? [{ label: "Add to list", on: !page.lists.key, id: null }, ...page.lists.status] : []} />
        {#if page.lists.score}<Select options={page.lists.score} />{/if}
      </div>
      {#if page.lists.progress}
        <div class="progress">
          <Button b={page.lists.progress.minus} small />
          <b>{page.lists.progress.text}</b>
          <Button b={page.lists.progress.plus} small />
          <span class="grow"></span>
          <Button b={page.lists.refresh} kind="link" small />
        </div>
      {/if}
      {#each page.lists.says as s}<p class="faint small">{s}</p>{/each}
    </div>
  {/if}

  {#if page.description}
    <div class="desc" class:more role="button" tabindex="0" onclick={() => (more = !more)} onkeydown={() => (more = !more)}>{@html page.description}</div>
  {/if}

  {#if page.library}
    {@const b = page.library}
    <div class="box">
      {#if b.t === "manage"}
        {@const p = b.panel}
        <div class="mhead"><span class="kind">{p.kind}</span><b>{p.title}</b></div>
        <p class="faint small">{p.place}</p>
        {#if p.links.length}
          <div class="tags">{#each p.links as l}<button class="link" onclick={() => press(l.id)}>{l.label}</button>{/each}</div>
        {/if}
        <Chips options={p.tabs} scroll />
        <Form items={p.items} />
      {:else}
        <b class:green={b.t === "in_library"}>{b.title}</b>
        <p class="muted small">{b.note}</p>
        {#if b.add}<Button b={b.add} />{/if}
        {#if b.remove}
          <div class="actions">
            <Button b={b.remove} small />
            <Button b={b.delete} kind="danger" small />
          </div>
        {/if}
        {#if b.languages}
          <button class="check" onclick={() => press(b.languages.toggle)}>
            <span class="tick" class:on={b.languages.own}>{b.languages.own ? "✓" : ""}</span>
            Different languages for this series
          </button>
          <p class="faint small">{b.languages.summary}</p>
        {/if}
      {/if}
    </div>
  {/if}

  <h2>Episodes <span class="muted small">{page.episodes.summary}</span></h2>
  {#if page.episodes.tools.length}
    <div class="actions">{#each page.episodes.tools as b}<Button {b} small />{/each}</div>
  {/if}
  {#if page.episodes.empty}<p class="muted">AniList doesn't list its episodes yet.</p>{/if}
  <div class="episodes">
    {#each page.episodes.rows as e (e.n)}
      <div class="ep" class:here={e.present}>
        <div class="top">
          {#if e.monitor}
            <button class="monitor" class:on={e.monitor.on} onclick={() => press(e.monitor.id)} aria-label="Watched for by Sonarr">
              {e.monitor.on ? "✓" : ""}
            </button>
          {/if}
          <span class="num" class:done={e.done}>{String(e.n).padStart(2, "0")}</span>
          <span class="what">
            <span class="title">{e.title}
              {#if e.filler}<span class="badge gold">Filler</span>{/if}
              {#if e.recap}<span class="badge">Recap</span>{/if}
              {#if e.listed}<span class="badge green" title="Watched, as your lists say">{e.listed}</span>{/if}
            </span>
            {#if e.tags.length}
              <span class="rtags">{#each e.tags as t}<span class="rt {t.tone}">{t.label}</span>{/each}</span>
            {/if}
          </span>
          {#if e.score}<span class="score">{e.score}</span>{/if}
        </div>
        {#if e.details}
          <div class="details"><Form items={e.details} /></div>
        {/if}
        {#if e.progress}
          <div class="watched"><span class="bar"><span style="width: {e.progress.fraction * 100}%"></span></span><span class="faint small">{e.progress.text}</span></div>
        {/if}
        <div class="bottom">
          {#if e.airs}<span class="faint small">{e.airs}</span>{/if}
          {#if e.state?.text}<span class="faint small">{e.state.text}</span>{/if}
          {#if e.state?.downloading !== undefined}
            <span class="dl"><span class="bar"><span style="width: {e.state.downloading * 100}%"></span></span>{Math.round(e.state.downloading * 100)}%</span>
          {/if}
          <span class="grow"></span>
          {#each e.buttons as b}<Button {b} small />{/each}
          {#if e.play?.label}<Button b={e.play} kind="primary" small />{/if}
        </div>
      </div>
    {/each}
  </div>
  {#if page.episodes.extras.length}
    <p class="label">Other files in the folder</p>
    {#each page.episodes.extras as x}<p class="faint small">{x.size} · {x.name}</p>{/each}
  {/if}

  <div class="box">
    <div class="rhead">
      <b>{release.title}</b>
      {#if release.chosen}
        <button class="link" onclick={() => (releaseOpen = !releaseOpen)}>{releaseOpen ? "Done" : "Change"}</button>
      {/if}
    </div>
    {#if showReleases}
      {#each release.notes as n}<p class="small {n.tone}">{n.text}</p>{/each}
      {#if release.search}
        <form class="search" onsubmit={(e) => { e.preventDefault(); if (release.search.go.id) press(release.search.go.id); }}>
          <div class="grow"><Field f={release.search.query} hint="Search Nyaa" /></div>
          <Button b={release.search.go} />
        </form>
      {/if}
      {#each release.cards as c}
        <button class="release" class:chosen={c.chosen} class:off={!c.choose} onclick={() => c.choose && press(c.choose)}>
          <span class="radio" class:on={c.chosen}></span>
          <span class="rinfo">
            <span class="rtop"><b>{c.group}</b>
              <span class="rank" class:best={c.best} class:blocked={c.blocked}>{c.rank}</span>
              {#if c.usenet}<span class="nzb">{c.usenet}</span>{/if}
            </span>
            {#if c.title}<span class="faint small rtitle">{c.title}</span>{/if}
            <span class="muted small">{c.facts.join(" · ")}</span>
            {#if c.problem}<span class="warn small">{c.problem}</span>{/if}
          </span>
        </button>
      {/each}
      {#if release.choose}
        <div class="choose"><Button b={release.choose.button} /><span class="muted small">{release.choose.note}</span></div>
      {/if}
      {#if release.notes_seadex}<p class="label">SeaDex notes</p><p class="muted small">{release.notes_seadex}</p>{/if}
    {/if}
  </div>

  {#if page.recommended?.length}
    <h2>Recommended <span class="muted small">by AniList's users</span></h2>
    <div class="recs">
      {#each page.recommended as m (m.id)}
        <button class="rec" onclick={() => press(m.open)}>
          <span class="rcover">
            <Poster src={m.cover} />
            {#if m.in_library}<span class="pill">In library</span>{/if}
          </span>
          <span class="rt">{m.title}</span>
          <span class="rm">{m.facts}</span>
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .recs { display: flex; gap: 10px; overflow-x: auto; margin: 0 -16px; padding: 0 16px 4px; scrollbar-width: none; }
  .rec { flex: none; width: 112px; display: flex; flex-direction: column; gap: 2px; padding: 0; border: none; background: none; color: inherit; text-align: left; font: inherit; }
  .rcover { position: relative; margin-bottom: 4px; }
  .rcover .pill { position: absolute; top: 6px; left: 6px; font-size: 10px; font-weight: 600; color: white; padding: 2px 7px; border-radius: 999px; background: var(--green); }
  .rt { font-size: 12px; font-weight: 600; line-height: 1.25; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
  .rm { font-size: 11px; color: var(--muted); }
  .hero { position: relative; height: 230px; overflow: hidden; background: var(--surface); }
  .banner { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; }
  .shade { position: absolute; inset: 0; background: linear-gradient(rgba(13, 15, 20, 0.35), rgba(13, 15, 20, 0.75) 55%, var(--bg)); }
  .head { position: absolute; left: 16px; right: 16px; bottom: 10px; display: flex; gap: 14px; align-items: flex-end; }
  .cover { width: 96px; flex: none; }
  .names { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .names b { font-size: 22px; line-height: 1.15; color: white; }
  .names span { font-size: 13px; color: var(--muted); }
  .page { padding: 10px 16px 28px; display: flex; flex-direction: column; gap: 12px; }
  .facts { margin: 0; font-size: 13px; color: #c9cedA; }
  .tags { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; }
  .genre { font-size: 12px; padding: 4px 10px; border-radius: 999px; background: rgba(255, 255, 255, 0.08); }
  .link { background: none; border: none; color: var(--accent); font: inherit; font-size: 13px; padding: 2px 4px; }
  .actions { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; }
  .box { background: var(--surface); border: 1px solid var(--line); border-radius: 12px; padding: 12px 14px; display: flex; flex-direction: column; gap: 8px; }
  .box p { margin: 0; }
  .green { color: var(--green); }
  .mhead { display: flex; align-items: center; gap: 8px; }
  .mhead b { font-size: 16px; }
  .kind { font-size: 11px; font-weight: 600; color: white; background: var(--green); padding: 2px 8px; border-radius: 999px; }
  .details { background: var(--surface2); border-radius: 10px; padding: 12px; }
  .lists .row2 { display: grid; grid-template-columns: 3fr 2fr; gap: 8px; }
  .progress { display: flex; align-items: center; gap: 10px; }
  .grow { flex: 1; }
  .desc { color: var(--muted); font-size: 14px; line-height: 1.55; display: -webkit-box; -webkit-line-clamp: 4; line-clamp: 4; -webkit-box-orient: vertical; overflow: hidden; }
  .desc.more { display: block; }
  .check { display: flex; align-items: center; gap: 10px; background: none; border: none; color: var(--text); font: inherit; font-size: 14px; padding: 0; text-align: left; }
  .tick { width: 20px; height: 20px; border-radius: 5px; border: 1px solid var(--line); background: var(--surface2); display: grid; place-items: center; font-size: 13px; flex: none; }
  .tick.on { background: var(--accent); border-color: var(--accent); color: white; }
  h2 { margin: 10px 0 0; display: flex; align-items: baseline; gap: 10px; flex-wrap: wrap; }
  .episodes { display: flex; flex-direction: column; gap: 6px; }
  .ep { background: rgba(23, 27, 36, 0.6); border-radius: 10px; padding: 10px 12px; display: flex; flex-direction: column; gap: 6px; }
  .ep.here { background: var(--surface); }
  .top { display: flex; gap: 10px; align-items: center; }
  .monitor { flex: none; width: 22px; height: 22px; border-radius: 6px; border: 1px solid var(--line); background: var(--surface2); color: white; font-size: 12px; padding: 0; }
  .monitor.on { background: var(--accent); border-color: var(--accent); }
  .num { flex: none; width: 34px; height: 34px; border-radius: 9px; background: var(--surface2); display: grid; place-items: center; font-size: 13px; font-weight: 600; color: var(--muted); }
  .num.done { color: var(--green); background: rgba(52, 211, 153, 0.12); }
  .what { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 3px; }
  .title { font-size: 14px; font-weight: 500; }
  .ep:not(.here) .title { color: var(--faint); }
  .badge { font-size: 10px; font-weight: 600; padding: 1px 6px; border-radius: 999px; background: var(--surface2); color: var(--muted); margin-left: 4px; }
  .badge.gold { color: var(--gold); }
  .badge.green { color: var(--green); }
  .rtags { display: flex; flex-wrap: wrap; gap: 5px; }
  .rt { font-size: 10px; font-weight: 600; padding: 1px 6px; border-radius: 999px; background: var(--surface2); color: var(--muted); }
  .rt.plain { background: none; padding: 1px 0; }
  .rt.accent { color: var(--accent); }
  .rt.good { color: var(--green); }
  .rt.warn { color: var(--gold); }
  .rt.faint { background: none; color: var(--faint); font-weight: 400; }
  .score { font-size: 12px; font-weight: 600; color: var(--gold); flex: none; }
  .watched { display: flex; align-items: center; gap: 8px; }
  .bar { display: inline-block; width: 120px; height: 3px; border-radius: 2px; background: var(--surface2); overflow: hidden; }
  .bar span { display: block; height: 100%; background: var(--accent); }
  .bottom { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; }
  .dl { display: flex; align-items: center; gap: 6px; font-size: 12px; font-weight: 600; color: var(--green); }
  .dl .bar span { background: var(--green); }
  .label { font-size: 12px; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; color: var(--faint); margin: 6px 0 0; }
  .rhead { display: flex; align-items: center; gap: 10px; }
  .rhead b { flex: 1; font-size: 15px; }
  .search { display: flex; gap: 8px; align-items: flex-end; }
  .release { display: flex; gap: 10px; padding: 10px; border-radius: 10px; border: 1px solid var(--line); background: var(--bg); color: inherit; font: inherit; text-align: left; }
  .release.chosen { border-color: var(--accent); background: rgba(139, 124, 255, 0.1); }
  .release.off { opacity: 0.6; }
  .radio { flex: none; width: 18px; height: 18px; border-radius: 9px; border: 1px solid var(--line); background: var(--surface2); margin-top: 2px; }
  .radio.on { background: var(--accent); border: 5px solid #c9c2ff; }
  .rinfo { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
  .rtop { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .rank { font-size: 11px; color: var(--muted); }
  .rank.best { color: var(--gold); font-weight: 600; }
  .rank.blocked { color: var(--red); }
  .nzb { font-size: 10px; font-weight: 600; color: var(--green); }
  .rtitle { word-break: break-word; }
  .warn { color: var(--gold); }
  .error { color: var(--red); }
  .faint { color: var(--faint); }
  .choose { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
</style>
