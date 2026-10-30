<script setup lang="ts">
// Runs of jobs, services and agents, with their logs.
import type { TableColumn } from "@nuxt/ui";
import { onMounted, ref, shallowRef, watch } from "vue";
import { useRoute } from "vue-router";
import { call } from "../api/client";
import RunLogsModal from "../components/common/RunLogsModal.vue";
import TimeAgo from "../components/common/TimeAgo.vue";
import ViewPanel from "../components/common/ViewPanel.vue";
import type { Run } from "../generated";
import { portUrl, runDuration, runStatusColor, serviceColor } from "../lib/runs";
import { notifyError } from "../notify";
import { state } from "../store";

const route = useRoute();
const location = window.location;
const runs = shallowRef<Run[] | null>(null);
const loading = ref(false);
const logsOf = ref<string | null>(null);

async function load() {
  loading.value = true;
  try {
    runs.value = await call("runs", {});
  } catch (e) {
    notifyError("Could not load runs", e);
  } finally {
    loading.value = false;
  }
}
onMounted(load);
watch(
  () => route.query.run,
  (id) => {
    if (typeof id === "string") logsOf.value = id;
  },
  { immediate: true },
);

const columns: TableColumn<Run>[] = [
  { accessorKey: "runnable", header: "Runnable" },
  { accessorKey: "kind", header: "Kind" },
  { accessorKey: "status", header: "Status" },
  { accessorKey: "exit_code", header: "Exit" },
  { id: "took", header: "Took" },
  { accessorKey: "created_at", header: "Started" },
  { accessorKey: "reason", header: "Reason" },
  { id: "logs", header: "" },
];
</script>

<template>
  <ViewPanel id="runs" title="Runs" icon="i-lucide-play" body-class="p-0 sm:p-0">
    <template #right>
      <UButton icon="i-lucide-refresh-cw" color="neutral" variant="ghost" aria-label="Refresh" :loading="loading" @click="load" />
    </template>
    <template v-if="state.status?.services.length" #toolbar>
      <div class="flex w-full flex-wrap items-center gap-2 py-2 text-sm" aria-label="Services">
        <span class="font-medium text-muted">Services</span>
        <span
          v-for="s in state.status.services"
          :key="s.name"
          class="inline-flex items-center gap-1 rounded-md border border-default px-1.5 py-0.5"
        >
          <UTooltip :text="s.failed ? 'Crash-looping: restarts once its incident is resolved' : s.running ? 'Running' : 'Stopped'">
            <UChip :color="serviceColor(s)" standalone inset size="sm" />
          </UTooltip>
          <span class="font-medium">{{ s.name }}</span>
          <UButton
            v-for="p in s.published"
            :key="p.host_port"
            :to="portUrl(p, location.hostname)"
            target="_blank"
            size="xs"
            variant="link"
            trailing-icon="i-lucide-external-link"
            :label="`:${p.host_port}`"
            class="px-0.5"
          />
        </span>
      </div>
    </template>
    <UTable :data="runs ?? []" :columns="columns" :loading="loading && !runs" sticky class="flex-1" :ui="{ td: 'py-2.5' }">
      <template #runnable-cell="{ row }"><span class="font-medium text-highlighted">{{ row.original.runnable }}</span></template>
      <template #status-cell="{ row }">
        <UBadge :color="runStatusColor(row.original.status)" variant="subtle" size="sm" :label="row.original.status" />
      </template>
      <template #exit_code-cell="{ row }"><span class="tabular-nums">{{ row.original.exit_code ?? "" }}</span></template>
      <template #took-cell="{ row }"><span class="tabular-nums text-muted">{{ runDuration(row.original) }}</span></template>
      <template #created_at-cell="{ row }"><TimeAgo :at="row.original.created_at" class="whitespace-nowrap text-muted" /></template>
      <template #reason-cell="{ row }"><span class="line-clamp-2 max-w-md text-muted">{{ row.original.reason ?? "" }}</span></template>
      <template #logs-cell="{ row }">
        <UButton size="xs" color="neutral" variant="outline" icon="i-lucide-scroll-text" label="Logs" @click="logsOf = row.original.id" />
      </template>
      <template #empty>
        <UEmpty icon="i-lucide-play" title="No runs yet" description="Jobs, services and agent containers appear here once they start." variant="naked" />
      </template>
    </UTable>
    <RunLogsModal v-model="logsOf" />
  </ViewPanel>
</template>
