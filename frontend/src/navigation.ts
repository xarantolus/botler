// The app's sections, for the sidebar and the command palette.

import { computed } from "vue";
import { state } from "./store";

export interface Section {
  name: string;
  label: string;
  icon: string;
  /** Something needs attention there (a count). */
  count?: number;
  countColor?: "warning" | "error" | "neutral";
}

const attention = computed(() => state.status?.attention);

export const sections = computed((): Section[] => [
  { name: "home", label: "Chat", icon: "i-lucide-message-square" },
  {
    name: "requests",
    label: "Requests",
    icon: "i-lucide-inbox",
    count: state.dialogs?.open.length ?? 0,
    countColor: "warning",
  },
  { name: "search", label: "Search", icon: "i-lucide-text-search" },
  { name: "events", label: "Events", icon: "i-lucide-activity" },
  {
    name: "incidents",
    label: "Incidents",
    icon: "i-lucide-siren",
    count: state.status?.open_incidents ?? 0,
    countColor: "error",
  },
  {
    // Failed (red) wins over backing off (yellow).
    name: "system",
    label: "System",
    icon: "i-lucide-server",
    count: (attention.value?.system_failed ?? 0) + (attention.value?.system_backoff ?? 0),
    countColor: attention.value?.system_failed ? "error" : "warning",
  },
  {
    // In flight; yellow when some wait for you.
    name: "proposals",
    label: "Proposals",
    icon: "i-lucide-git-pull-request-arrow",
    count: attention.value?.proposals_in_flight ?? 0,
    countColor: attention.value?.proposals_awaiting_user ? "warning" : "neutral",
  },
  ...(state.apps?.length ? [{ name: "apps", label: "Apps", icon: "i-lucide-app-window" }] : []),
  { name: "secrets", label: "Secrets", icon: "i-lucide-key-round" },
  { name: "memory", label: "Memory", icon: "i-lucide-brain" },
  { name: "settings", label: "Settings", icon: "i-lucide-settings" },
]);
