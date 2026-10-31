<script setup lang="ts">
// What the user should know about a chat before reading it: how it was archived, breakers
// pausing it or holding its messages, waking progress or failure, and that it waits for them.
import { computed } from "vue";
import type { AgentStatus, Instance } from "../../generated";
import { heldFor, pausing } from "../../lib/breakers";
import type { Badge } from "../../lib/phase";
import { state } from "../../store";
import BreakerResume from "../common/BreakerResume.vue";
import MarkdownText from "../common/MarkdownText.vue";
import TimeAgo from "../common/TimeAgo.vue";
import WakeProgress from "./WakeProgress.vue";

const props = defineProps<{ instance: Instance; status: AgentStatus | null; badge: Badge | null }>();
const emit = defineEmits<{ showRequest: [] }>();

const breakers = computed(() => state.status?.breakers ?? []);
const held = computed(() => heldFor(props.instance.name, breakers.value));
const paused = computed(() => pausing(props.instance.name, breakers.value));
const report = computed(() => props.instance.archive_report);
const sleepingForYou = computed(() => props.badge?.label === "waiting for you");
/** The applied definitions do not load: the hub runs without some of them. */
const problems = computed(() => state.status?.definition_problems ?? []);
const skipped = computed(() => state.status?.skipped_definitions ?? []);
</script>

<template>
  <div class="flex flex-col gap-2.5">
    <UAlert
      v-if="problems.length"
      color="error"
      variant="subtle"
      icon="i-lucide-triangle-alert"
      :title="`The monorepo does not load: running without ${skipped.join(', ') || 'some definitions'}`"
    >
      <template #description>
        <ul class="list-disc ps-5">
          <li v-for="p in problems" :key="p.path + p.message"><code class="font-mono">{{ p.path }}</code>: {{ p.message }}</li>
        </ul>
        <p class="mt-1">The front door was told; a proposal that fixes these brings them back.</p>
      </template>
    </UAlert>
    <UAlert
      v-if="report"
      color="neutral"
      variant="subtle"
      icon="i-lucide-archive"
      :title="report.confirmed_by_agent ? 'Archived' : 'Archived without the agent\'s handover'"
    >
      <template #description>
        <div class="space-y-2">
          <template v-if="report.report">
            <MarkdownText :source="report.report.summary" />
            <div v-if="report.report.outstanding.length">
              <div class="font-medium text-highlighted">Outstanding</div>
              <ul class="list-disc ps-5">
                <li v-for="(o, i) in report.report.outstanding" :key="i">{{ o }}</li>
              </ul>
            </div>
            <div v-if="report.report.state.length">
              <div class="font-medium text-highlighted">Where its state lives</div>
              <ul class="list-disc ps-5">
                <li v-for="(o, i) in report.report.state" :key="i">{{ o }}</li>
              </ul>
            </div>
          </template>
          <p v-if="report.reason">Reason: {{ report.reason }}</p>
          <p v-if="report.handed_to">
            Handed to
            <ULink :to="`/chat/${encodeURIComponent(report.handed_to)}`" class="font-medium text-primary hover:underline">
              {{ report.handed_to }}
            </ULink>
          </p>
          <p class="text-xs text-dimmed"><TimeAgo :at="report.at" /> · sending it a message restores it</p>
        </div>
      </template>
    </UAlert>
    <UAlert
      v-else-if="instance.archive_requested_at"
      color="warning"
      variant="subtle"
      icon="i-lucide-hourglass"
      title="Handing over"
    >
      <template #description>
        Waiting for the agent to hand over before archiving (asked <TimeAgo :at="instance.archive_requested_at" />).
      </template>
    </UAlert>

    <UAlert
      v-for="b in paused"
      :key="b.subject"
      color="error"
      variant="subtle"
      icon="i-lucide-octagon-pause"
      title="Paused by a breaker"
      :description="`${b.reason}. It does not wake until you resume it.`"
      orientation="horizontal"
    >
      <template #actions><BreakerResume :subject="b.subject" size="xs" /></template>
    </UAlert>
    <UAlert
      v-for="h in held"
      :key="h.breaker.subject"
      color="warning"
      variant="subtle"
      icon="i-lucide-pause"
      title="Messages are held"
      orientation="horizontal"
    >
      <template #description>
        Messages between this chat and <strong>{{ h.other }}</strong> are held by a breaker: {{ h.breaker.reason }}
      </template>
      <template #actions><BreakerResume :subject="h.breaker.subject" size="xs" /></template>
    </UAlert>

    <WakeProgress :instance="instance.name" :status="status" :starting="instance.state === 'starting'" />

    <UAlert
      v-if="sleepingForYou"
      color="warning"
      variant="subtle"
      icon="i-lucide-hand"
      :title="badge?.detail ?? 'Waiting for your answer'"
      description="It continues once you answer them."
      orientation="horizontal"
      :actions="[
        { label: 'Show', color: 'warning', variant: 'outline', size: 'xs', onClick: () => emit('showRequest') },
        { label: 'All requests', color: 'warning', variant: 'ghost', size: 'xs', to: '/requests' },
      ]"
    />
  </div>
</template>
