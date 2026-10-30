<script setup lang="ts">
// Runs of jobs, services and agents, newest first, with their logs: all of them (System's
// History tab) or one runnable's (its slideover).
import type { TableColumn } from "@nuxt/ui";
import { ref, shallowRef, watch } from "vue";
import { call } from "../../api/client";
import type { Run } from "../../generated";
import { runDuration, runStatusColor } from "../../lib/runs";
import { notifyError } from "../../notify";
import RunLogsModal from "../common/RunLogsModal.vue";
import TimeAgo from "../common/TimeAgo.vue";

const props = defineProps<{ runnable?: string; compact?: boolean }>();
/** The run whose logs are open. */
const logsOf = defineModel<string | null>("logs", { default: null });
const runs = shallowRef<Run[] | null>(null);
const loading = ref(false);

async function load() {
  loading.value = true;
  try {
    runs.value = await call("runs", { query: props.runnable ? { runnable: props.runnable } : {} });
  } catch (e) {
    notifyError("Could not load runs", e);
  } finally {
    loading.value = false;
  }
}
watch(() => props.runnable, load, { immediate: true });
defineExpose({ load, loading });

const columns: TableColumn<Run>[] = [
  ...(props.runnable ? [] : [{ accessorKey: "runnable", header: "Runnable" }, { accessorKey: "kind", header: "Kind" }]),
  { accessorKey: "status", header: "Status" },
  ...(props.compact ? [] : [{ accessorKey: "exit_code", header: "Exit" }]),
  { id: "took", header: "Took" },
  { accessorKey: "created_at", header: "Started" },
  ...(props.compact ? [] : [{ accessorKey: "reason", header: "Reason" }]),
  { id: "logs", header: "" },
];
</script>

<template>
  <div>
    <UTable :data="runs ?? []" :columns="columns" :loading="loading && !runs" sticky class="flex-1" :ui="{ td: 'py-2.5' }">
      <template #runnable-cell="{ row }">
        <ULink :to="{ name: 'system', query: { job: row.original.runnable } }" class="font-medium text-highlighted hover:underline">
          {{ row.original.runnable }}
        </ULink>
      </template>
      <template #status-cell="{ row }">
        <UBadge :color="runStatusColor(row.original.status)" variant="subtle" size="sm" :label="row.original.status" />
      </template>
      <template #exit_code-cell="{ row }"><span class="tabular-nums">{{ row.original.exit_code ?? "" }}</span></template>
      <template #took-cell="{ row }"><span class="tabular-nums text-muted">{{ runDuration(row.original) }}</span></template>
      <template #created_at-cell="{ row }"><TimeAgo :at="row.original.created_at" class="whitespace-nowrap text-muted" /></template>
      <template #reason-cell="{ row }"><span class="line-clamp-2 max-w-md whitespace-normal text-muted">{{ row.original.reason ?? "" }}</span></template>
      <template #logs-cell="{ row }">
        <UButton size="xs" color="neutral" variant="outline" icon="i-lucide-scroll-text" label="Logs" @click="logsOf = row.original.id" />
      </template>
      <template #empty>
        <UEmpty icon="i-lucide-play" title="No runs yet" :description="runnable ? 'It has not run yet.' : 'Jobs, services and agent containers appear here once they start.'" variant="naked" />
      </template>
    </UTable>
    <RunLogsModal v-model="logsOf" />
  </div>
</template>
