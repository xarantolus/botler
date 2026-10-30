<script setup lang="ts">
// The hub at a glance (sidebar footer): live connection, today's spend, breakers, the kill
// switch, appearance, version and logging out.
import type { DropdownMenuItem } from "@nuxt/ui";
import { computed, ref } from "vue";
import { call } from "../../api/client";
import { confirm } from "../../confirm";
import { usd } from "../../lib/time";
import { notifyError, notifySuccess } from "../../notify";
import { logout, refreshStatus, state } from "../../store";
import BreakerResume from "../common/BreakerResume.vue";

defineProps<{ collapsed?: boolean }>();

const s = computed(() => state.status);
const toggling = ref(false);

const connection = computed(() =>
  state.connection === "live"
    ? { label: "Live", color: "success" as const, help: "Changes stream in over the live connection." }
    : state.connection === "polling"
      ? { label: "Polling", color: "warning" as const, help: "The live connection is down; refreshing every few seconds while reconnecting." }
      : { label: "Connecting", color: "neutral" as const, help: "Connecting to the hub." },
);

const spend = computed(() => {
  const u = state.usage;
  if (!u) return null;
  return { today: u.today.cost_usd, tokens: u.today.tokens, budget: u.default_budget.usd_per_day, tz: u.timezone };
});

async function togglePause() {
  const paused = s.value?.paused ?? false;
  const ok = await confirm(
    paused
      ? { title: "Resume botler?", description: "Schedules, dispatch and services start again.", confirmLabel: "Resume" }
      : {
          title: "Pause everything?",
          description: "This stops all agents and services and freezes schedules and dispatch until you resume.",
          confirmLabel: "Pause everything",
          color: "error",
        },
  );
  if (!ok) return;
  toggling.value = true;
  try {
    await call(paused ? "resume" : "pause", {});
    notifySuccess(paused ? "Resumed" : "Paused", paused ? "botler is running again." : "Everything is stopped until you resume.");
    await refreshStatus();
  } catch (e) {
    notifyError(paused ? "Resume failed" : "Pause failed", e);
  } finally {
    toggling.value = false;
  }
}

const items = computed((): DropdownMenuItem[][] => [
  [
    {
      type: "label",
      label: s.value ? `botler v${s.value.version} · ${s.value.commit.slice(0, 10)}` : "botler",
      icon: "i-lucide-info",
    },
    ...(s.value ? [{ type: "label" as const, label: `${s.value.runtime.binary} ${s.value.runtime.version}`, icon: "i-lucide-container" }] : []),
  ],
  [
    {
      label: s.value?.paused ? "Resume everything" : "Pause everything",
      icon: s.value?.paused ? "i-lucide-play" : "i-lucide-octagon-pause",
      color: s.value?.paused ? "primary" : "error",
      disabled: !s.value || toggling.value,
      onSelect: () => void togglePause(),
    },
  ],
  [{ label: "Log out", icon: "i-lucide-log-out", onSelect: logout }],
]);
</script>

<template>
  <div class="flex w-full min-w-0 flex-col gap-2">
    <div v-if="!collapsed && s" class="flex flex-wrap items-center gap-1.5 px-1">
      <UTooltip :text="connection.help">
        <UBadge :color="connection.color" variant="subtle" size="sm" class="gap-1.5">
          <span class="size-1.5 rounded-full bg-current" :class="state.connection === 'live' && 'animate-pulse'" />
          {{ connection.label }}
        </UBadge>
      </UTooltip>
      <UBadge v-if="s.paused" color="error" variant="solid" size="sm" icon="i-lucide-octagon-pause" label="Paused" />
      <UPopover v-if="s.breakers.length" :content="{ side: 'top', align: 'start' }">
        <UButton
          color="error"
          variant="subtle"
          size="xs"
          icon="i-lucide-zap-off"
          :label="`${s.breakers.length} breaker${s.breakers.length === 1 ? '' : 's'}`"
        />
        <template #content>
          <div class="max-w-sm space-y-3 p-3">
            <div class="text-sm font-semibold text-highlighted">Tripped breakers</div>
            <div v-for="b in s.breakers" :key="b.subject" class="flex items-start gap-3 text-sm">
              <div class="min-w-0 flex-1">
                <div class="font-medium">{{ b.subject }} <span class="text-xs text-muted">({{ b.kind }})</span></div>
                <div class="text-muted">{{ b.reason }}</div>
              </div>
              <BreakerResume :subject="b.subject" size="xs" />
            </div>
          </div>
        </template>
      </UPopover>
      <UTooltip v-if="spend">
        <span class="ms-auto text-xs tabular-nums text-muted">today {{ usd(spend.today) }}</span>
        <template #content>
          <span>
            {{ spend.tokens.toLocaleString() }} tokens since midnight ({{ spend.tz }}).
            <template v-if="spend.budget !== null">Agents without their own budget stop at {{ usd(spend.budget) }} a day.</template>
          </span>
        </template>
      </UTooltip>
    </div>
    <UDropdownMenu :items="items" :content="{ side: 'top', align: 'start' }" :ui="{ content: 'w-(--reka-dropdown-menu-trigger-width) min-w-56' }">
      <UButton
        color="neutral"
        variant="ghost"
        block
        :square="collapsed"
        class="data-[state=open]:bg-elevated"
        :class="!collapsed && 'justify-start'"
        :aria-label="collapsed ? 'Hub menu' : undefined"
        :ui="{ trailingIcon: 'text-dimmed ms-auto' }"
        :trailing-icon="collapsed ? undefined : 'i-lucide-chevrons-up-down'"
      >
        <UChip :color="connection.color" size="sm" position="bottom-right" inset>
          <UAvatar icon="i-lucide-bot" size="xs" class="bg-primary/15 text-primary" />
        </UChip>
        <span v-if="!collapsed" class="truncate">Hub{{ s?.paused ? " (paused)" : "" }}</span>
      </UButton>
    </UDropdownMenu>
  </div>
</template>
