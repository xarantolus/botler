<script setup lang="ts">
// While an agent wakes: which step it is on and for how long. When waking failed (or the agent
// crashed), why, with the container's stderr when the hub reported it.
import { NAlert, NSpin, NStep, NSteps, NText } from "naive-ui";
import { computed, onBeforeUnmount, ref } from "vue";
import type { AgentStatus } from "../generated";
import { asBuiltin } from "../lib/events";
import { WAKE_STEPS } from "../lib/phase";
import { elapsed } from "../lib/time";
import { state } from "../store";

const props = defineProps<{ instance: string; status: AgentStatus | null; starting: boolean }>();

const now = ref(Date.now());
const timer = window.setInterval(() => (now.value = Date.now()), 1000);
onBeforeUnmount(() => window.clearInterval(timer));

const waking = computed(() => props.status?.status === "waking" || (props.starting && !props.status));
const current = computed(() => {
  if (props.status?.status !== "waking") return 1;
  return WAKE_STEPS.findIndex((s) => s.step === (props.status as { step: string }).step) + 1;
});
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
</script>

<template>
  <div v-if="waking" class="wake">
    <div style="display: flex; align-items: center; gap: 10px; margin-bottom: 10px">
      <NSpin :size="18" />
      <strong>Waking the agent</strong>
      <NText v-if="since" depth="3">{{ since }}</NText>
    </div>
    <NSteps :current="current" size="small" status="process">
      <NStep v-for="s in WAKE_STEPS" :key="s.step" :title="s.label" />
      <NStep title="Delivering" />
    </NSteps>
    <NText depth="3" style="font-size: 12px; display: block; margin-top: 8px">
      The first start on a new host builds the agent image, which can take a few minutes.
    </NText>
  </div>
  <NAlert v-else-if="status?.status === 'failed'" type="error" title="The agent could not start" class="wake">
    <pre class="mono">{{ status.error }}</pre>
    <template v-if="crash?.p.logs_tail">
      <div style="font-size: 12px; margin-top: 6px">Last output ({{ crash.p.kind }}, ×{{ crash.p.count }}):</div>
      <pre class="mono">{{ crash.p.logs_tail }}</pre>
    </template>
    <NText depth="3" style="font-size: 12px">It retries with the next message.</NText>
  </NAlert>
  <NAlert v-else-if="status?.status === 'paused'" type="error" title="Paused by a breaker" class="wake">
    {{ status.reason }}
  </NAlert>
  <NAlert
    v-else-if="recentCrash && recentCrash.p.logs_tail && status === null"
    type="warning"
    title="It crashed in the last hour"
    class="wake"
    closable
  >
    <pre class="mono">{{ recentCrash.p.logs_tail }}</pre>
  </NAlert>
</template>

<style scoped>
.wake {
  margin: 10px 16px 0;
  padding: 12px 14px;
  border-radius: 8px;
  border: 1px solid rgba(127, 127, 127, 0.25);
}
</style>
