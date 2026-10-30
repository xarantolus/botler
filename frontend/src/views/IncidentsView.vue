<script setup lang="ts">
import { NButton, NDataTable, NTag, type DataTableColumns } from "naive-ui";
import { h, onMounted, ref, shallowRef } from "vue";
import { call } from "../api/client";
import RunLogs from "../components/RunLogs.vue";
import type { Incident } from "../generated";
import { ago } from "../lib/time";
import { notifyError } from "../notify";

const incidents = shallowRef<Incident[]>([]);
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

const columns: DataTableColumns<Incident> = [
  { title: "Runnable", key: "runnable", ellipsis: { tooltip: true } },
  { title: "Kind", key: "kind", width: 100, render: (i) => h(NTag, { size: "small", type: "error", bordered: false }, () => i.kind) },
  { title: "Count", key: "count", width: 70 },
  { title: "Since", key: "first_at", width: 110, render: (i) => h("span", { title: i.first_at }, ago(i.first_at)) },
  { title: "Last", key: "last_at", width: 110, render: (i) => h("span", { title: i.last_at }, ago(i.last_at)) },
  {
    title: "Status",
    key: "status",
    width: 90,
    render: (i) => h(NTag, { size: "small", bordered: false, type: i.status === "open" ? "warning" : "success" }, () => i.status),
  },
  {
    title: "",
    key: "logs",
    width: 90,
    render: (i) =>
      i.last_run ? h(NButton, { size: "tiny", onClick: () => (logsOf.value = i.last_run) }, () => "Last logs") : "",
  },
];
</script>

<template>
  <div style="height: 100%; padding: 8px 12px; display: flex; flex-direction: column; min-height: 0; gap: 8px">
    <div><NButton size="small" :loading="loading" @click="load">Refresh</NButton></div>
    <NDataTable
      :columns="columns"
      :data="incidents"
      :loading="loading"
      :row-key="(i: Incident) => i.id"
      size="small"
      flex-height
      style="flex: 1; min-height: 0"
      :scroll-x="700"
    />
    <RunLogs v-model="logsOf" />
  </div>
</template>
