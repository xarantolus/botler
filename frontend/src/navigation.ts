// The app's sections, for the sidebar and the command palette.

import { computed } from "vue";
import { state } from "./store";

export interface Section {
  name: string;
  label: string;
  icon: string;
  /** Something needs attention there (a count). */
  count?: number;
  countColor?: "warning" | "error";
}

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
  { name: "runs", label: "Runs", icon: "i-lucide-play" },
  {
    name: "incidents",
    label: "Incidents",
    icon: "i-lucide-siren",
    count: state.status?.open_incidents ?? 0,
    countColor: "error",
  },
  { name: "proposals", label: "Proposals", icon: "i-lucide-git-pull-request-arrow" },
  { name: "secrets", label: "Secrets", icon: "i-lucide-key-round" },
  { name: "memory", label: "Memory", icon: "i-lucide-brain" },
]);
