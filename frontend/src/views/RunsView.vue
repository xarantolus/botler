<script setup lang="ts">
import { NButton, NDataTable, NTag, type DataTableColumns } from "naive-ui";
import { h, onMounted, ref, watch, shallowRef } from "vue";
import { useRoute } from "vue-router";
import { call } from "../api/client";
import RunLogs from "../components/RunLogs.vue";
import type { Run, RunStatus } from "../generated";
import { ago } from "../lib/time";
import { notifyError } from "../notify";

const route = useRoute();
const runs = shallowRef<Run[]>([]);
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

const statusType = (s: RunStatus) =>
  s === "succeeded" ? "success" : s === "running" || s === "queued" ? "info" : s === "cancelled" ? "default" : "error";

function duration(r: Run): string {
  if (!r.started_at) return "";
  const end = r.finished_at ? new Date(r.finished_at).getTime() : Date.now();
  const s = Math.round((end - new Date(r.started_at).getTime()) / 1000);
  return s < 120 ? `${s}s` : `${Math.round(s / 60)} min`;
}

const columns: DataTableColumns<Run> = [
  { title: "Runnable", key: "runnable", ellipsis: { tooltip: true } },
  { title: "Kind", key: "kind", width: 90 },
  {
    title: "Status",
    key: "status",
    width: 110,
    render: (r) => h(NTag, { size: "small", bordered: false, type: statusType(r.status) }, () => r.status),
  },
  { title: "Exit", key: "exit_code", width: 60, render: (r) => (r.exit_code === null ? "" : String(r.exit_code)) },
  { title: "Took", key: "took", width: 80, render: duration },
  { title: "Started", key: "created_at", width: 110, render: (r) => h("span", { title: r.created_at }, ago(r.created_at)) },
  { title: "Reason", key: "reason", ellipsis: { tooltip: true }, render: (r) => r.reason ?? "" },
  {
    title: "",
    key: "logs",
    width: 70,
    render: (r) => h(NButton, { size: "tiny", onClick: () => (logsOf.value = r.id) }, () => "Logs"),
  },
];
</script>

<template>
  <div style="height: 100%; padding: 8px 12px; display: flex; flex-direction: column; min-height: 0; gap: 8px">
    <div><NButton size="small" :loading="loading" @click="load">Refresh</NButton></div>
    <NDataTable
      :columns="columns"
      :data="runs"
      :loading="loading"
      :row-key="(r: Run) => r.id"
      size="small"
      flex-height
      style="flex: 1; min-height: 0"
      :scroll-x="820"
    />
    <RunLogs v-model="logsOf" />
  </div>
</template>
