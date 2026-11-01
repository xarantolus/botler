<script setup lang="ts">
// The chats in the sidebar: Botler first, then the ones the user pinned, then those waiting for
// the user, then by when someone last wrote in them. Each shows what it is doing when that is
// worth a glance; its menu pins, renames or archives it.
import type { DropdownMenuItem, NavigationMenuItem } from "@nuxt/ui";
import { archiveNow, pinChat, renameChat } from "../../chatActions";
import { computed } from "vue";
import type { Instance } from "../../generated";
import { chatOrder } from "../../lib/chatOrder";
import { chatTitle } from "../../lib/events";
import { phaseBadge, type Badge } from "../../lib/phase";
import { usd } from "../../lib/time";
import { shell } from "../../shell";
import { frontDoorName, state, waitingByInstance } from "../../store";
import PhaseBadge from "../common/PhaseBadge.vue";
import TimeAgo from "../common/TimeAgo.vue";

const selected = computed(() => state.session?.instance.name ?? null);
const waitingChats = computed(() => Object.keys(waitingByInstance.value).length);

const chats = computed(() => {
  const door = frontDoorName.value;
  const shown = state.instances.filter((i) => i.state !== "archived" || state.showArchived || i.name === selected.value);
  return chatOrder(shown, door, waitingByInstance.value);
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

function actions(i: Instance): DropdownMenuItem[] {
  const title = chatTitle(i, frontDoorName.value);
  const out: DropdownMenuItem[] = [
    { label: i.pinned ? "Unpin" : "Pin", icon: i.pinned ? "i-lucide-pin-off" : "i-lucide-pin", onSelect: () => void pinChat(i.name, !i.pinned) },
    { label: "Rename…", icon: "i-lucide-pencil", onSelect: () => void renameChat(i.name, title) },
  ];
  if (i.name !== frontDoorName.value && i.state !== "archived") {
    out.push({ label: "Archive now…", icon: "i-lucide-archive", onSelect: () => void archiveNow(i.name) });
  }
  return out;
}

const items = computed((): ChatItem[] =>
  chats.value.map((i) => {
    const b = badge(i);
    const cost = state.usage?.total_by_instance[i.name]?.cost_usd;
    const door = i.name === frontDoorName.value;
    return {
      label: chatTitle(i, frontDoorName.value),
      to: `/chat/${encodeURIComponent(i.slug ?? i.name)}`,
      active: i.name === selected.value,
      icon: door ? "i-lucide-star" : i.pinned ? "i-lucide-pin" : i.state === "archived" ? "i-lucide-archive" : "i-lucide-message-square",
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
      <span class="flex items-center gap-1.5 text-xs font-semibold text-muted uppercase tracking-wide">
        Chats
        <UTooltip v-if="waitingChats" :text="`${waitingChats} waiting for you`">
          <UBadge color="warning" variant="subtle" size="sm" :label="String(waitingChats)" />
        </UTooltip>
      </span>
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
              <TimeAgo :at="(item as ChatItem).instance.last_message_at" />
            </span>
          </span>
        </span>
      </template>
      <template #item-trailing="{ item }">
        <UDropdownMenu :items="actions((item as ChatItem).instance)" :content="{ align: 'end' }">
          <UButton
            icon="i-lucide-ellipsis"
            color="neutral"
            variant="ghost"
            size="xs"
            :aria-label="`Actions for ${item.label}`"
            class="opacity-60 hover:opacity-100"
            @click.stop.prevent
          />
        </UDropdownMenu>
      </template>
    </UNavigationMenu>

    <USwitch v-model="state.showArchived" label="Show archived" size="sm" class="mt-1 ps-2.5" />
  </div>
</template>
