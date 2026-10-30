<script setup lang="ts">
// The command palette (Cmd/Ctrl+K): jump to a chat, a request or a section, or act.
import type { CommandPaletteGroup, CommandPaletteItem } from "@nuxt/ui";
import { computed, ref } from "vue";
import { useRouter } from "vue-router";
import { chatTitle } from "../../lib/events";
import { sections } from "../../navigation";
import { shell } from "../../shell";
import { frontDoorName, logout, state, waitingByInstance } from "../../store";

const router = useRouter();
const searchTerm = ref("");

const groups = computed((): CommandPaletteGroup<CommandPaletteItem>[] => {
  const door = frontDoorName.value;
  const chats = [...state.instances]
    .filter((i) => i.state !== "archived" || state.showArchived)
    .sort((a, b) => (a.name === door ? -1 : b.name === door ? 1 : b.last_active.localeCompare(a.last_active)))
    .map((i) => ({
      id: `chat:${i.name}`,
      label: chatTitle(i, door),
      suffix: i.name === chatTitle(i, door) ? i.definition : i.name,
      icon: i.name === door ? "i-lucide-star" : "i-lucide-message-square",
      chip: waitingByInstance.value[i.name] ? { color: "warning" as const } : undefined,
      to: `/chat/${encodeURIComponent(i.name)}`,
    }));
  const requests = (state.dialogs?.open ?? []).map((d) => ({
    id: `dialog:${d.id}`,
    label: d.question,
    suffix: d.instance,
    icon: d.kind === "approval" ? "i-lucide-shield-question" : d.kind === "credentials" ? "i-lucide-key-round" : "i-lucide-message-circle-question",
    to: `/chat/${encodeURIComponent(d.instance)}`,
  }));
  const term = searchTerm.value.trim();
  return [
    // Typing offers the full-text search (conversations, memory, events, the monorepo) first.
    ...(term
      ? [
          {
            id: "everything",
            label: "Search everything",
            ignoreFilter: true,
            items: [
              {
                label: `Search everything for “${term}”`,
                icon: "i-lucide-text-search",
                onSelect: () => void router.push({ name: "search", query: { q: term } }),
              },
            ],
          },
        ]
      : []),
    {
      id: "actions",
      label: "Actions",
      items: [
        { label: "New chat", icon: "i-lucide-square-pen", onSelect: () => (shell.newChat = true) },
        { label: "Log out", icon: "i-lucide-log-out", onSelect: logout },
      ],
    },
    ...(requests.length ? [{ id: "requests", label: "Waiting for you", items: requests }] : []),
    { id: "chats", label: "Chats", items: chats },
    {
      id: "sections",
      label: "Go to",
      items: sections.value.map((s) => ({
        label: s.label,
        icon: s.icon,
        onSelect: () => void router.push({ name: s.name }),
      })),
    },
  ];
});
</script>

<template>
  <UDashboardSearch v-model:search-term="searchTerm" :groups="groups" placeholder="Search chats, requests, sections, or everything…" />
</template>
