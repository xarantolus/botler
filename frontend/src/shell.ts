// UI state of the app shell shared across components (not app data: that is store.ts).

import { reactive } from "vue";

export const shell = reactive({
  /** The "new chat" modal is open. */
  newChat: false,
  /** The mobile sidebar (drawer) is open. */
  sidebar: false,
  /** The session tree of the chat on screen is open. */
  sessionTree: false,
  /** The command palette is open. */
  palette: false,
  /** What is typed in the command palette. */
  paletteTerm: "",
  /** Open tool calls by `toolCallId`, and thinking blocks by `<turn key>:<part>`; outlives the
   * components, so the switch from a streamed to a persisted message keeps what is open. */
  open: {} as Record<string, boolean>,
  /** Thinking blocks the user opened or closed: they are never closed automatically. */
  touched: {} as Record<string, boolean>,
});
