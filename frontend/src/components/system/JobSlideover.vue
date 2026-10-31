<script setup lang="ts">
// One job or service: its definition in words, its open incident and its run history with logs.
import { computed } from "vue";
import type { JobView } from "../../generated";
import { portUrl } from "../../lib/runs";
import { fireLabel, plain, serviceStateColor } from "../../lib/system";
import TimeAgo from "../common/TimeAgo.vue";
import RunnableAction from "./RunnableAction.vue";
import RunsTable from "./RunsTable.vue";

const props = defineProps<{ job: JobView | null; timezone: string; paused: boolean }>();
const emit = defineEmits<{ close: []; changed: [] }>();
const logsOf = defineModel<string | null>("logs", { default: null });
const hostname = window.location.hostname;
const open = computed(() => props.job !== null);
</script>

<template>
  <USlideover
    :open="open"
    :title="job?.name ?? ''"
    :description="job?.description || (job?.lifetime === 'service' ? 'Service' : 'Job')"
    :ui="{ content: 'sm:max-w-2xl', body: 'space-y-5' }"
    @update:open="(o: boolean) => !o && emit('close')"
  >
    <template v-if="job" #body>
      <div class="flex flex-wrap items-center gap-2">
        <UBadge color="neutral" variant="subtle" :label="job.lifetime === 'service' ? 'service' : 'oneshot job'" />
        <UBadge v-if="job.service" :color="serviceStateColor(job.service.state)" variant="subtle" :label="job.service.state" />
        <UBadge v-for="s in job.secrets" :key="s" color="neutral" variant="outline" icon="i-lucide-key-round" :label="s" />
        <RunnableAction :job="job" :paused="paused" size="sm" class="ms-auto" @done="(id) => { emit('changed'); if (id) logsOf = id; }" />
      </div>

      <UAlert
        v-if="job.incident"
        color="error"
        variant="subtle"
        icon="i-lucide-siren"
        :title="`Open incident (${job.incident.kind}), ${job.incident.count}×`"
        :actions="[{ label: 'Incidents', to: { name: 'incidents' }, color: 'error', variant: 'outline', size: 'xs' }]"
      >
        <template #description>Last failed <TimeAgo :at="job.incident.last_at" />.</template>
      </UAlert>

      <section class="space-y-2">
        <h3 class="text-sm font-semibold text-highlighted">Triggers</h3>
        <p v-if="!job.triggers.length" class="text-sm text-muted">
          {{ job.lifetime === "service" ? "Services run continuously." : "Nothing triggers it; it runs when started by hand or by an agent." }}
        </p>
        <div v-for="t in job.triggers" :key="t.id" class="rounded-md border border-default p-3 text-sm">
          <div class="font-medium text-highlighted">{{ plain(t.when) }}</div>
          <div class="text-muted">{{ plain(t.then) }} <UBadge v-if="t.shadow" color="neutral" variant="outline" size="sm" label="shadow" /></div>
          <ul v-if="t.upcoming.length" class="mt-1.5 space-y-0.5 text-xs tabular-nums text-muted">
            <li v-for="at in t.upcoming" :key="at">{{ fireLabel(at, timezone) }}</li>
          </ul>
          <div class="mt-1 truncate text-xs text-dimmed">{{ t.id }}</div>
        </div>
      </section>

      <section v-if="job.service" class="space-y-2">
        <h3 class="text-sm font-semibold text-highlighted">Addresses</h3>
        <div class="flex flex-wrap gap-2 text-sm">
          <code v-if="job.service.internal_url" class="text-muted">{{ job.service.internal_url }}</code>
          <UButton
            v-for="p in job.service.published"
            :key="p.host_port"
            :to="portUrl(p, hostname)"
            target="_blank"
            size="xs"
            variant="link"
            trailing-icon="i-lucide-external-link"
            :label="portUrl(p, hostname)"
          />
        </div>
      </section>

      <section class="space-y-2">
        <h3 class="text-sm font-semibold text-highlighted">Runs</h3>
        <RunsTable :key="job.name + job.recent_runs[0]?.id" v-model:logs="logsOf" :runnable="job.name" compact />
      </section>
    </template>
  </USlideover>
</template>
