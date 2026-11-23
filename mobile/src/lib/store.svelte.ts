// The view, kept up to date: asked for again whenever the core says it
// changed (one request at a time; a change during one asks once more).
import { listen } from "@tauri-apps/api/event";
import { getView, type View } from "./api";

export const app = $state<{
  view: View | null;
  note: string | null;
  /** The settings group open (More), and its name. */
  section: { key: string; name: string } | null;
}>({
  view: null,
  note: null,
  section: null,
});

let busy = false;
let again = false;

export async function refresh() {
  if (busy) {
    again = true;
    return;
  }
  busy = true;
  try {
    do {
      again = false;
      app.view = await getView();
    } while (again);
  } finally {
    busy = false;
  }
}

export function start() {
  refresh();
  listen("changed", () => refresh());
}

/** Say something briefly (a greyed-out button's reason). */
export function say(text: string) {
  app.note = text;
  setTimeout(() => {
    if (app.note === text) app.note = null;
  }, 3500);
}
