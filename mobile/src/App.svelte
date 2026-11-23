<script lang="ts">
  // The app's frame: a bar on top (the page's name, a way back, search),
  // the page, and four tabs along the bottom.
  import { onBackButtonPress } from "@tauri-apps/api/app";
  import { app, start } from "./lib/store.svelte";
  import { back, dismissNote, goTab, type View } from "./lib/api";
  import Library from "./pages/Library.svelte";
  import More from "./pages/More.svelte";

  start();
  // Back: out of an open settings group first, then the app's own.
  function goBack() {
    if (app.section) app.section = null;
    else back();
  }
  onBackButtonPress(goBack);

  const tabs: { key: View["tab"]; icon: string; name: string }[] = [
    { key: "library", icon: "▦", name: "Library" },
    { key: "discover", icon: "✦", name: "Discover" },
    { key: "downloads", icon: "↓", name: "Downloads" },
    { key: "more", icon: "☰", name: "More" },
  ];

  const names: Record<string, string> = {
    library: "Library",
    discover: "Discover",
    downloads: "Downloads",
    more: "More",
    search: "Search",
    files: "Choose files",
    remux: "Remux",
    playlist: "Playlist",
  };
  const deep = ["series", "files", "remux", "playlist", "search"];

  let page = $derived(app.view?.page);
  let title = $derived(
    page?.kind === "series"
      ? page.title
      : page?.kind === "more" && app.section
        ? app.section.name
        : (names[page?.kind] ?? ""),
  );
  let deeper = $derived(deep.includes(page?.kind) || (page?.kind === "more" && !!app.section));
  let note = $derived(app.note ?? app.view?.note?.text ?? null);
  let noteError = $derived(!app.note && !!app.view?.note?.error);

  // Each page keeps where it was scrolled to.
  let scroller: HTMLElement | undefined = $state();
  const scrolled = new Map<string, number>();
  let key = $derived(page ? page.kind + (page.id ?? "") + (app.section?.key ?? "") : "");
  let lastKey = "";
  $effect(() => {
    const k = key;
    if (!scroller || k === lastKey) return;
    lastKey = k;
    const top = scrolled.get(k) ?? 0;
    requestAnimationFrame(() => scroller && (scroller.scrollTop = top));
  });
</script>

<div class="frame">
  <header>
    {#if deeper}
      <button class="icon" onclick={goBack} aria-label="Back">←</button>
    {:else}
      <span class="gap"></span>
    {/if}
    <h1>{title}</h1>
  </header>

  {#if note}
    <button class="note" class:error={noteError} onclick={() => (app.note ? (app.note = null) : dismissNote())}>
      <span class="dot"></span>{note}
    </button>
  {/if}

  <main bind:this={scroller} onscroll={() => scroller && scrolled.set(key, scroller.scrollTop)}>
    {#if !page}
      <p class="muted center">Starting…</p>
    {:else if page.kind === "library"}
      <Library {page} />
    {:else if page.kind === "more"}
      <More {page} />
    {:else}
      <p class="muted center">{page.kind}: coming soon</p>
    {/if}
  </main>

  <nav>
    {#each tabs as t}
      <button
        class="tab"
        class:on={app.view?.tab === t.key}
        onclick={() => {
          app.section = null;
          goTab(t.key);
        }}
      >
        <span class="pill">
          <span class="ti">{t.icon}</span>
          {#if t.key === "downloads" && (app.view?.downloading ?? 0) > 0}
            <span class="badge">{app.view?.downloading}</span>
          {/if}
        </span>
        <span class="tn">{t.name}</span>
      </button>
    {/each}
  </nav>
</div>

<style>
  .frame { display: flex; flex-direction: column; height: 100%; }
  header {
    display: flex; align-items: center; gap: 4px;
    height: 56px; padding: 0 8px; background: var(--bars); flex: none;
  }
  header h1 {
    font-size: 20px; font-weight: 700; margin: 0;
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis; flex: 1;
  }
  .gap { width: 8px; }
  .icon {
    width: 40px; height: 40px; border-radius: 20px; border: none;
    background: none; color: var(--text); font-size: 20px;
  }
  .icon:active { background: var(--surface2); }
  main {
    flex: 1; overflow-y: auto; overflow-x: hidden;
    -webkit-overflow-scrolling: touch; overscroll-behavior: contain;
  }
  .center { text-align: center; margin-top: 30vh; }
  nav {
    display: flex; flex: none; background: var(--bars);
    border-top: 1px solid var(--line); padding: 2px 4px;
  }
  .tab {
    flex: 1; display: flex; flex-direction: column; align-items: center; gap: 4px;
    padding: 8px 0; border: none; background: none; color: var(--muted);
    font: inherit; font-size: 12px; font-weight: 500;
  }
  .tab .pill {
    display: flex; align-items: center; gap: 4px;
    padding: 4px 18px; border-radius: 999px;
  }
  .tab .ti { font-size: 18px; line-height: 1; }
  .tab.on { color: var(--text); font-weight: 600; }
  .tab.on .pill { background: rgba(139, 124, 255, 0.18); }
  .tab.on .ti { color: var(--accent); }
  .badge {
    font-size: 10px; font-weight: 600; color: var(--text);
    background: rgba(139, 124, 255, 0.6); border-radius: 999px; padding: 0 5px;
  }
  .note {
    position: fixed; left: 12px; right: 12px; top: calc(56px + 8px); z-index: 20;
    display: flex; align-items: center; gap: 10px; text-align: left;
    background: var(--surface2); color: var(--text); border: 1px solid var(--line);
    border-radius: 10px; padding: 10px 14px; font: inherit; font-size: 13px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.5);
  }
  .note .dot { width: 8px; height: 8px; border-radius: 4px; background: var(--green); flex: none; }
  .note.error .dot { background: var(--red); }
</style>
