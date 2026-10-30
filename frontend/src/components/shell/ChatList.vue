<script setup lang="ts">
// The chats in the sidebar: Botler pinned first, then those waiting for the user, then the most
// recently active. Each shows what it is doing when that is worth a glance.
import type { NavigationMenuItem } from "@nuxt/ui";
import { computed } from "vue";
import type { Instance } from "../../generated";
import { chatTitle } from "../../lib/events";
import { phaseBadge, type Badge } from "../../lib/phase";
import { usd } from "../../lib/time";
import { shell } from "../../shell";
import { frontDoorName, state, waitingByInstance } from "../../store";
import PhaseBadge from "../common/PhaseBadge.vue";
import TimeAgo from "../common/TimeAgo.vue";

const selected = computed(() => state.session?.instance.name ?? null);

const chats = computed(() => {
  const door = frontDoorName.value;
  return state.instances
    .filter((i) => i.state !== "archived" || state.showArchived || i.name === selected.value)
    .sort((a, b) => {
      if (a.name === door) return -1;
      if (b.name === door) return 1;
      const wa = waitingByInstance.value[a.name] ? 1 : 0;
      const wb = waitingByInstance.value[b.name] ? 1 : 0;
      return wb - wa || b.last_active.localeCompare(a.last_active);
    });
});

function badge(i: Instance): Badge {
  return phaseBadge({
    state: i.state,
    status: state.liveStatus[i.name] ?? null,
    waitingOnUser: waitingByInstance.value[i.name],
    archiveRequested: i.archive_requested_at !== null,
  });
}

/** Badges worth showing in the list (not the everyday running / sleeping / archived). */
function notable(b: Badge): boolean {
  return !["running", "sleeping", "archived"].includes(b.label);
}

type ChatItem = NavigationMenuItem & { instance: Instance; badgeInfo: Badge; sub: string };

const items = computed((): ChatItem[] =>
  chats.value.map((i) => {
    const b = badge(i);
    const cost = state.usage?.total_by_instance[i.name]?.cost_usd;
    const door = i.name === frontDoorName.value;
    return {
      label: chatTitle(i, frontDoorName.value),
      to: `/chat/${encodeURIComponent(i.name)}`,
      active: i.name === selected.value,
      icon: door ? "i-lucide-star" : i.state === "archived" ? "i-lucide-archive" : "i-lucide-message-square",
      chip: waitingByInstance.value[i.name] ? { color: "warning" as const, size: "md" as const } : undefined,
      instance: i,
      badgeInfo: b,
      // The definition only when it is not the usual one; the star marks the front door.
      sub: [door || i.definition === "chat" ? "" : i.definition, cost ? usd(cost) : ""].filter(Boolean).join(" · "),
    };
  }),
);
</script>

<template>
  <div class="flex min-h-0 flex-col gap-1">
    <div class="flex items-center justify-between ps-2.5">
      <span class="text-xs font-semibold text-muted uppercase tracking-wide">Chats</span>
      <UTooltip text="New chat">
        <UButton icon="i-lucide-square-pen" color="neutral" variant="ghost" size="sm" aria-label="New chat" @click="shell.newChat = true" />
      </UTooltip>
    </div>

    <UEmpty
      v-if="!items.length"
      icon="i-lucide-messages-square"
      title="No chats yet"
      size="sm"
      variant="naked"
      :actions="[{ label: 'New chat', icon: 'i-lucide-plus', size: 'sm', onClick: () => (shell.newChat = true) }]"
    />
    <UNavigationMenu
      v-else
      :items="items"
      orientation="vertical"
      color="primary"
      :ui="{ link: 'py-2 items-start', linkLeadingIcon: 'mt-0.5', linkLabel: 'w-full' }"
      aria-label="Chats"
    >
      <template #item-label="{ item }">
        <span class="flex min-w-0 flex-col">
          <span class="truncate" :title="(item as ChatItem).instance.name">{{ item.label }}</span>
          <span class="mt-0.5 flex min-w-0 items-center gap-1.5 text-xs font-normal text-dimmed">
            <PhaseBadge v-if="notable((item as ChatItem).badgeInfo)" :badge="(item as ChatItem).badgeInfo" />
            <span class="truncate">
              <template v-if="(item as ChatItem).sub">{{ (item as ChatItem).sub }} · </template>
              <TimeAgo :at="(item as ChatItem).instance.last_active" />
            </span>
          </span>
        </span>
      </template>
    </UNavigationMenu>

    <USwitch v-model="state.showArchived" label="Show archived" size="sm" class="mt-1 ps-2.5" />
  </div>
</template>
