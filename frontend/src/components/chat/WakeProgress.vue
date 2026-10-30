<script setup lang="ts">
// While an agent wakes: which step it is on and for how long. When waking failed (or the agent
// crashed), why, with the container's stderr when the hub reported it.
import { computed, onBeforeUnmount, ref } from "vue";
import type { AgentStatus } from "../../generated";
import { asBuiltin } from "../../lib/events";
import { WAKE_STEPS } from "../../lib/phase";
import { elapsed } from "../../lib/time";
import { state } from "../../store";
import CodeBlock from "../common/CodeBlock.vue";

const props = defineProps<{ instance: string; status: AgentStatus | null; starting: boolean }>();

const now = ref(Date.now());
const timer = window.setInterval(() => (now.value = Date.now()), 1000);
onBeforeUnmount(() => window.clearInterval(timer));

const waking = computed(() => props.status?.status === "waking" || (props.starting && !props.status));
const steps = [
  ...WAKE_STEPS.map((s) => ({ value: s.step as string, title: s.label })),
  { value: "delivering", title: "Delivering" },
];
const current = computed(() => (props.status?.status === "waking" ? props.status.step : WAKE_STEPS[0]!.step));
const since = computed(() => {
  const t = state.wakeStarted[props.instance];
  return t ? elapsed(now.value - t) : null;
});

/** The latest failure the hub reported for this agent (with its stderr tail). */
const crash = computed(() => {
  for (const ev of state.events) {
    const b = asBuiltin(ev);
    if (b?.type === "runnable.failed" && b.payload.runnable === `agent:${props.instance}`) return { ev, p: b.payload };
  }
  return null;
});
const recentCrash = computed(() =>
  crash.value && now.value - new Date(crash.value.ev.at).getTime() < 3600_000 ? crash.value : null,
);
const crashDismissed = ref<string | null>(null);
</script>

<template>
  <div v-if="waking" class="rounded-lg border border-default bg-elevated/40 p-3 sm:p-4" role="status" aria-live="polite">
    <div class="mb-3 flex items-center gap-2 text-sm">
      <UIcon name="i-lucide-loader-circle" class="size-4 animate-spin text-primary" />
      <span class="font-medium text-highlighted">Waking the agent</span>
      <span v-if="since" class="text-muted tabular-nums">{{ since }}</span>
    </div>
    <UStepper :items="steps" :model-value="current" size="xs" disabled class="w-full" />
    <p class="mt-3 text-xs text-muted">The first start on a new host builds the agent image, which can take a few minutes.</p>
  </div>
  <UAlert
    v-else-if="status?.status === 'failed'"
    color="error"
    variant="subtle"
    icon="i-lucide-server-crash"
    title="The agent could not start"
  >
    <template #description>
      It retries with the next message.
      <UCollapsible class="mt-1.5">
        <UButton
          color="error"
          variant="link"
          size="xs"
          label="Details"
          trailing-icon="i-lucide-chevron-down"
          class="px-0"
          :ui="{ trailingIcon: 'group-data-[state=open]:rotate-180 transition-transform' }"
        />
        <template #content>
          <div class="mt-1.5 space-y-2">
            <CodeBlock :text="status.error" error max-height="10rem" />
            <CodeBlock
              v-if="crash?.p.logs_tail"
              :label="`Last output (${crash.p.kind}, ×${crash.p.count})`"
              :text="crash.p.logs_tail"
              max-height="12rem"
            />
          </div>
        </template>
      </UCollapsible>
    </template>
  </UAlert>
  <UAlert
    v-else-if="status?.status === 'paused'"
    color="error"
    variant="subtle"
    icon="i-lucide-octagon-pause"
    title="Paused by a breaker"
    :description="status.reason"
  />
  <UAlert
    v-else-if="recentCrash && recentCrash.p.logs_tail && status === null && crashDismissed !== recentCrash.ev.id"
    color="warning"
    variant="subtle"
    icon="i-lucide-triangle-alert"
    title="It crashed in the last hour"
    close
    @update:open="crashDismissed = recentCrash.ev.id"
  >
    <template #actions>
      <CodeBlock :text="recentCrash.p.logs_tail" class="w-full" max-height="10rem" />
    </template>
  </UAlert>
</template>
