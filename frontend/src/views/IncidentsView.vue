<script setup lang="ts">
// Runnables that keep failing, with the logs of their last run.
import type { TableColumn } from "@nuxt/ui";
import { onMounted, ref, shallowRef } from "vue";
import { call } from "../api/client";
import RunLogsModal from "../components/common/RunLogsModal.vue";
import TimeAgo from "../components/common/TimeAgo.vue";
import ViewPanel from "../components/common/ViewPanel.vue";
import type { Incident } from "../generated";
import { incidentStatusColor } from "../lib/runs";
import { notifyError } from "../notify";

const incidents = shallowRef<Incident[] | null>(null);
const loading = ref(false);
const logsOf = ref<string | null>(null);

async function load() {
  loading.value = true;
  try {
    incidents.value = await call("incidents", {});
  } catch (e) {
    notifyError("Could not load incidents", e);
  } finally {
    loading.value = false;
  }
}
onMounted(load);

const columns: TableColumn<Incident>[] = [
  { accessorKey: "runnable", header: "Runnable" },
  { accessorKey: "kind", header: "Kind" },
  { accessorKey: "count", header: "Count" },
  { accessorKey: "first_at", header: "Since" },
  { accessorKey: "last_at", header: "Last" },
  { accessorKey: "status", header: "Status" },
  { id: "logs", header: "" },
];
</script>

<template>
  <ViewPanel id="incidents" title="Incidents" icon="i-lucide-siren" body-class="p-0 sm:p-0">
    <template #right>
      <UButton icon="i-lucide-refresh-cw" color="neutral" variant="ghost" aria-label="Refresh" :loading="loading" @click="load" />
    </template>
    <UTable :data="incidents ?? []" :columns="columns" :loading="loading && !incidents" sticky class="flex-1" :ui="{ td: 'py-2.5' }">
      <template #runnable-cell="{ row }">
        <span class="font-medium text-highlighted">{{ row.original.runnable }}</span>
        <p v-if="row.original.resolution" class="text-xs text-muted">{{ row.original.resolution }}</p>
      </template>
      <template #kind-cell="{ row }"><UBadge color="error" variant="subtle" size="sm" :label="row.original.kind" /></template>
      <template #count-cell="{ row }"><span class="tabular-nums">×{{ row.original.count }}</span></template>
      <template #first_at-cell="{ row }"><TimeAgo :at="row.original.first_at" class="whitespace-nowrap text-muted" /></template>
      <template #last_at-cell="{ row }"><TimeAgo :at="row.original.last_at" class="whitespace-nowrap text-muted" /></template>
      <template #status-cell="{ row }">
        <UBadge :color="incidentStatusColor(row.original.status)" variant="subtle" size="sm" :label="row.original.status" />
      </template>
      <template #logs-cell="{ row }">
        <UButton
          v-if="row.original.last_run"
          size="xs"
          color="neutral"
          variant="outline"
          icon="i-lucide-scroll-text"
          label="Last logs"
          @click="logsOf = row.original.last_run"
        />
      </template>
      <template #empty>
        <UEmpty icon="i-lucide-shield-check" title="No incidents" description="Nothing keeps failing." variant="naked" />
      </template>
    </UTable>
    <RunLogsModal v-model="logsOf" />
  </ViewPanel>
</template>
