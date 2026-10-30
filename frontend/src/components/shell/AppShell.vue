<script setup lang="ts">
// The dashboard: a collapsible, resizable sidebar (a drawer on phones) with the sections, the
// chats and the hub's status; the current view beside it; the command palette and the new-chat
// modal on top.
import type { NavigationMenuItem } from "@nuxt/ui";
import { defineShortcuts } from "@nuxt/ui/composables/defineShortcuts";
import { computed } from "vue";
import { useRoute, useRouter } from "vue-router";
import { sections } from "../../navigation";
import { shell } from "../../shell";
import { SHORTCUTS } from "../../shortcuts";
import AppSearch from "./AppSearch.vue";
import ChatList from "./ChatList.vue";
import ColorModeCycle from "./ColorModeCycle.vue";
import HubMenu from "./HubMenu.vue";
import NewChatModal from "./NewChatModal.vue";

const route = useRoute();
const router = useRouter();
defineShortcuts(Object.fromEntries(SHORTCUTS.map((s) => [s.key, () => s.run(router)])));

/** Move focus past the sidebar, to the view's scrolling body. */
function skipToContent() {
  const body = document.querySelector<HTMLElement>("[data-slot=body]:not([data-slot=sidebar] *)");
  if (!body) return;
  body.tabIndex = -1;
  body.focus();
}

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
    <!-- Keyboard users skip the sidebar. -->
    <a
      href="#main"
      @click.prevent="skipToContent"
      class="sr-only z-50 rounded-md bg-default px-3 py-2 text-sm font-medium text-primary ring ring-primary focus:not-sr-only focus:fixed focus:start-3 focus:top-3"
    >
      Skip to content
    </a>
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
        <ColorModeCycle v-if="!collapsed" class="ms-auto" />
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
