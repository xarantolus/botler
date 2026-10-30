// Notifications usable outside components (API failures from background refreshes included).

import { createDiscreteApi, darkTheme } from "naive-ui";
import { computed, ref } from "vue";

export const dark = ref(false);

const { message, notification, dialog } = createDiscreteApi(["message", "notification", "dialog"], {
  configProviderProps: computed(() => ({ theme: dark.value ? darkTheme : null })),
});

export { dialog, message, notification };

const recent = new Map<string, number>();

/** Show an error to the user. Repeats of the same failure (e.g. polling) are shown once a minute. */
export function notifyError(what: string, e: unknown): void {
  const text = e instanceof Error ? e.message : String(e);
  const key = `${what}: ${text}`;
  const now = Date.now();
  if ((recent.get(key) ?? 0) > now - 60_000) return;
  recent.set(key, now);
  notification.error({ title: what, content: text, duration: 8000, keepAliveOnHover: true });
}
