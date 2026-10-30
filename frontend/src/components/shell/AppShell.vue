<script setup lang="ts">
// The dashboard: a collapsible, resizable sidebar (a drawer on phones) with the sections, the
// chats and the hub's status; the current view beside it; the command palette and the new-chat
// modal on top.
import type { NavigationMenuItem } from "@nuxt/ui";
import { computed } from "vue";
import { useRoute } from "vue-router";
import { sections } from "../../navigation";
import { shell } from "../../shell";
import AppSearch from "./AppSearch.vue";
import ChatList from "./ChatList.vue";
import HubMenu from "./HubMenu.vue";
import NewChatModal from "./NewChatModal.vue";

const route = useRoute();

const nav = computed((): NavigationMenuItem[] =>
  sections.value.map((s) => ({
    label: s.label,
    icon: s.icon,
    to: { name: s.name },
    active: s.name === "home" ? route.name === "home" || route.name === "chat" : route.name === s.name,
    badge: s.count ? { label: String(s.count), color: s.countColor, variant: "subtle" as const, size: "sm" as const } : undefined,
  })),
);
</script>

<template>
  <UDashboardGroup unit="rem" storage="local" storage-key="botler.dashboard">
    <UDashboardSidebar
      id="sidebar"
      v-model:open="shell.sidebar"
      collapsible
      resizable
      :min-size="15"
      :default-size="18"
      :max-size="26"
      class="bg-elevated/25"
      :ui="{ footer: 'lg:border-t lg:border-default', body: 'gap-5' }"
    >
      <template #header="{ collapsed }">
        <RouterLink to="/" class="flex min-w-0 items-center gap-2 rounded-md" aria-label="botler home">
          <img src="/favicon.svg" alt="" class="size-6 shrink-0" />
          <span v-if="!collapsed" class="truncate text-base font-semibold text-highlighted">botler</span>
        </RouterLink>
        <UColorModeButton v-if="!collapsed" class="ms-auto" size="sm" />
      </template>

      <template #default="{ collapsed }">
        <UDashboardSearchButton :collapsed="collapsed" class="bg-transparent ring-default" label="Search…" />
        <UNavigationMenu
          :collapsed="collapsed"
          :items="nav"
          orientation="vertical"
          tooltip
          popover
          aria-label="Sections"
        />
        <ChatList v-if="!collapsed" />
      </template>

      <template #footer="{ collapsed }">
        <HubMenu :collapsed="collapsed" />
      </template>
    </UDashboardSidebar>

    <RouterView />

    <AppSearch />
    <NewChatModal />
  </UDashboardGroup>
</template>
