// Notifications usable outside components (API failures from background refreshes included).
// App.vue binds Nuxt UI's toaster at startup; anything raised before that is shown then.

import type { Toast, useToast } from "@nuxt/ui/composables/useToast";

type Toaster = ReturnType<typeof useToast>;

let toaster: Toaster | null = null;
const early: Partial<Toast>[] = [];

export function bindToaster(t: Toaster): void {
  toaster = t;
  for (const toast of early.splice(0)) t.add(toast);
}

function show(toast: Partial<Toast>): void {
  if (toaster) toaster.add(toast);
  else early.push(toast);
}

/** The message of an error, for people. */
export function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

const recent = new Map<string, number>();

/** Show an error to the user. Repeats of the same failure (e.g. polling) are shown once a minute. */
export function notifyError(what: string, e: unknown): void {
  const text = errorText(e);
  const key = `${what}: ${text}`;
  const now = Date.now();
  if ((recent.get(key) ?? 0) > now - 60_000) return;
  recent.set(key, now);
  show({ title: what, description: text, color: "error", icon: "i-lucide-circle-alert", duration: 8000 });
}

export function notifySuccess(title: string, description?: string): void {
  show({ title, description, color: "success", icon: "i-lucide-circle-check", duration: 4000 });
}

export function notifyWarning(title: string, description?: string): void {
  show({ title, description, color: "warning", icon: "i-lucide-triangle-alert", duration: 8000 });
}
