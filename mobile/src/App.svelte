<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";

  let view: any = $state(null);

  async function refresh() {
    view = await invoke("view");
  }

  $effect(() => {
    refresh();
    const stop = listen("changed", refresh);
    return () => { stop.then((f) => f()); };
  });
</script>

<main>
  <h1>SeaDex</h1>
  <pre>{JSON.stringify(view, null, 2)}</pre>
</main>
