// UI state of the app shell shared across components (not app data: that is store.ts).

import { reactive } from "vue";

export const shell = reactive({
  /** The "new chat" modal is open. */
  newChat: false,
  /** The mobile sidebar (drawer) is open. */
  sidebar: false,
});
