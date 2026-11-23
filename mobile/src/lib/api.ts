// Talking to the app's core (Rust, through Tauri): the view it shows, and
// commands back. Buttons and fields in the view carry handles (`id`): the
// core keeps what each one does.
import { invoke } from "@tauri-apps/api/core";

/** A button: its label, and its handle (null: greyed out, maybe saying why). */
export type Btn = { label: string; id: string | null; why?: string };

/** A field: its text, and the handle typing into it sets. */
export type Field = { value: string; id: string };

export type Note = { text: string; error: boolean } | null;

export type View = {
  page: any;
  /** Opens the search page. */
  search: string;
  /** The dialog open (a sheet), if any. */
  dialog: any;
  tab: "library" | "discover" | "downloads" | "more";
  servers: boolean;
  downloading: number;
  note: Note;
};

type Command =
  | { do: "press"; id: string }
  | { do: "input"; id: string; value: string }
  | { do: "tab"; tab: View["tab"] }
  | { do: "back" }
  | { do: "dismiss_note" }
  | { do: "played"; id: number; episode: number; position: number; duration: number; then: string | null };

export function command(c: Command): Promise<any> {
  return invoke("command", { command: c });
}

export const press = (id: string) => command({ do: "press", id });
export const typed = (id: string, value: string) => command({ do: "input", id, value });
export const goTab = (tab: View["tab"]) => command({ do: "tab", tab });
/** Back: the app's; from its first page, the app goes to the background. */
export async function back() {
  const r = await command({ do: "back" });
  if (r?.exit) await invoke("to_background");
}
export const dismissNote = () => command({ do: "dismiss_note" });

export function getView(): Promise<View> {
  return invoke("view");
}
