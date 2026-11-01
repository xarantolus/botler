<script setup lang="ts">
// The command palette (Cmd/Ctrl+K): jump to a chat, a request or a section, or act.
import type { CommandPaletteGroup, CommandPaletteItem } from "@nuxt/ui";
import { computed } from "vue";
import { useRoute, useRouter } from "vue-router";
import { handoverChat, stopChat } from "../../chatActions";
import { chatName, chatTitle } from "../../lib/events";
import { sections } from "../../navigation";
import { shell } from "../../shell";
import { BUILTIN_SHORTCUTS, SHORTCUTS } from "../../shortcuts";
import { frontDoorName, logout, state, waitingByInstance } from "../../store";

const router = useRouter();
const route = useRoute();
const currentInstance = computed(() => state.instances.find((i) => i.name === currentChat.value) ?? null);
/** The chat on screen, if any. */
const currentChat = computed(() =>
  route.name === "chat" && typeof route.params.instance === "string"
    ? chatName(route.params.instance, state.instances, frontDoorName.value)
    : route.name === "home"
      ? frontDoorName.value
      : null,
);
const searchTerm = computed({ get: () => shell.paletteTerm, set: (v: string) => (shell.paletteTerm = v) });

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
      to: `/chat/${encodeURIComponent(i.slug ?? i.name)}`,
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
        ...(currentChat.value
          ? [{ label: "Stop this chat…", suffix: currentChat.value, icon: "i-lucide-octagon-x", onSelect: () => void stopChat(currentChat.value!) }]
          : []),
        ...(currentInstance.value?.persistent && currentInstance.value.state !== "archived"
          ? [
              {
                label: "Start fresh session (handover)…",
                suffix: currentChat.value!,
                icon: "i-lucide-refresh-ccw-dot",
                onSelect: () => void handoverChat(currentChat.value!),
              },
            ]
          : []),
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
        suffix: s.count ? `${s.count}` : undefined,
        onSelect: () => void router.push({ name: s.name }),
      })),
    },
    {
      id: "shortcuts",
      label: "Keyboard shortcuts",
      items: [
        ...SHORTCUTS.map((s) => ({ label: s.label, suffix: "shortcut", kbds: s.kbds, icon: "i-lucide-keyboard", onSelect: () => s.run(router) })),
        ...BUILTIN_SHORTCUTS.map((s) => ({ label: s.label, suffix: "shortcut", kbds: s.kbds, icon: "i-lucide-keyboard", disabled: true })),
      ],
    },
  ];
});
</script>

<template>
  <UDashboardSearch v-model:open="shell.palette" v-model:search-term="searchTerm" :groups="groups" placeholder="Search chats, requests, sections, or everything…" />
</template>
